use binance_async::Binance;
use binance_async::models::*;
use binance_async::rest::spot::GetAccountRequest;
#[allow(unused)]
use binance_async::rest::usdm::{self, StartUserDataStreamRequest, StartUserDataStreamResponse};

use binance_async::websocket::{BinanceWebsocket, usdm::WebsocketMessage};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use strum::IntoEnumIterator;

use serde::Deserialize;
use serde_json::Value;

use anyhow::{Context, Result, anyhow};
use derive_debug::Dbg;
use futures::StreamExt;
use num::FromPrimitive;
use num::ToPrimitive;
use reqwest;
use rust_decimal::Decimal;
use serde_json::json;
use websockets::WebSocket;

use core::pin::{pin};
use futures::stream::{FuturesUnordered};

use crate::data::{
    AssetData, Intv, Kline as KlineMine, Klines, get_asset_bases_binance, validate_asset_binance,
};
use crate::gui::{KeysStatus, LiveInfo, Settings};
use crate::trade::Order;
use crate::{BinInstructs, BinResponse, GeneralError};

use chrono::{DateTime, Utc};

const ERR_CTX: &str = "Binance client | websocket:";

#[allow(non_snake_case)]
#[derive(Deserialize, Debug, Clone)]
pub struct SymbolInfo {
    pub symbol: String,
    pub status: String,
    pub baseAsset: String,
    pub baseAssetPrecision: i64,
    pub quoteAsset: String,
    pub quotePrecision: i64,
    pub quoteAssetPrecision: i64,
    pub baseCommissionPrecision: i64,
    pub quoteCommissionPrecision: i64,
    //filters: Value
}
#[allow(non_snake_case)]
#[derive(Deserialize, Debug)]
pub struct FutSymbolInfo {
    pub symbol: String,
    pub status: String,
    pub deliveryDate: i64,
    pub onboardDate: i64,
    pub baseAsset: String,
    pub baseAssetPrecision: i64,
    pub quoteAsset: String,
    pub quotePrecision: i64,
    /*
    pub quoteAssetPrecision:	i64,
    pub baseCommissionPrecision:	i64,
    pub quoteCommissionPrecision:	i64,
    */
    //filters: Value
}

pub async fn fut_get_exchange_info() -> Result<Vec<FutSymbolInfo>> {
    let body = reqwest::get("https://fapi.binance.com/fapi/v1/exchangeInfo")
        .await?
        .text()
        .await?;
    let json_body: Value = serde_json::from_str(&body)?;
    let sym = json_body["symbols"].clone();
    let sym2 = serde_json::to_string(&sym)?;
    let symbols: Vec<FutSymbolInfo> = serde_json::from_str(&sym2)?;
    //TODO add progress bars
    Ok(symbols)
}

pub async fn get_exchange_info() -> Result<Vec<SymbolInfo>> {
    let body = reqwest::get("https://api.binance.com/api/v3/exchangeInfo")
        .await?
        .text()
        .await?;
    let json_body: Value = serde_json::from_str(&body)?;
    let sym = json_body["symbols"].clone();
    let sym2 = serde_json::to_string(&sym)?;
    let symbols: Vec<SymbolInfo> = serde_json::from_str(&sym2)?;
    Ok(symbols)
}
//Live data only for now ~ 20MB download
/*
[
  [
    1499040000000,      // Kline open time
    "0.01634790",       // Open price
    "0.80000000",       // High price
    "0.01575800",       // Low price
    "0.01577100",       // Close price
    "148976.11427815",  // Volume
    1499644799999,      // Kline Close time
    "2434.19055334",    // Quote asset volume
    308,                // Number of trades
    "1756.87402397",    // Taker buy base asset volume
    "28.46694368",      // Taker buy quote asset volume
    "0"                 // Unused field, ignore.
  ]
]
*/

#[allow(unused)]
#[allow(non_snake_case)]
#[derive(Deserialize, Debug)]
struct GetKline {
    open_time: i64,
    o: String,
    h: String,
    l: String,
    c: String,
    v: String,
    close_time: i64,
    q: String,
    no: i64,
    bb: String,
    bq: String,
    i: String,
}
impl GetKline {
    fn to_kline(input: &[GetKline]) -> KlineMine {
        let res: Vec<(DateTime<Utc>, f64, f64, f64, f64, f64)> = input
            .iter()
            .map(|n| {
                (
                    DateTime::<Utc>::from_timestamp_millis(n.open_time)
                        .expect("GETKLINE NOPE NOPE NOPE"),
                    n.o.as_str().parse().expect("Unable to parse value"),
                    n.h.as_str().parse().expect("Unable to parse value"),
                    n.l.as_str().parse().expect("Unable to parse value"),
                    n.c.as_str().parse().expect("Unable to parse value"),
                    n.v.as_str().parse().expect("Unable to parse value"),
                )
            })
            .collect();
        KlineMine::new_sql(res)
    }
}
async fn get_latest_wicks2(
    client: &reqwest::Client,
    symbol: &str,
    i: &Intv,
)->(KlineMine,Intv){
    let k = get_latest_wicks(&client, &symbol, i.to_bin_str()).await;
    let kl = match k {
        Ok(kli) => kli,
        Err(e) => {
            tracing::error![
                "Unable to get inital kline for interval: {} {}",
                i.to_str(),
                e
            ];
            let mut kl = vec![];
            for n in 0..5 {
                let res = get_latest_wicks(&client, &symbol, i.to_bin_str()).await;
                match res {
                    Ok(k) => {
                        kl = k;
                        break;
                    }
                    Err(e) => {
                        tracing::error!["unable to get inital kline {}, retry {}/5", e, n];
                    }
                };
            }
            kl
        }
    };
    (GetKline::to_kline(&kl),*i)
}

async fn get_latest_wicks(
    client: &reqwest::Client,
    symbol: &str,
    interval: &str,
) -> Result<Vec<GetKline>> {
    let res: String = client
        .get(&format![
            "https://api.binance.com/api/v3/klines?symbol={}&interval={}",
            symbol, interval
        ])
        .send()
        .await?
        .text()
        .await?;
    if res.is_empty() != true {
        //tracing::debug!["GET response:{}", &res];
        let res = serde_json::from_str(&res);
        match res {
            Ok(symbols) => return Ok(symbols),
            Err(e) => {
                tracing::error!["Get latest wicks error {}", e];
                return Err(e.into());
            }
        };
    } else {
        return Err(anyhow!["GET response empty (Get latest wicks)"]);
    };
}

#[allow(non_snake_case)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct OrderBookWS {
    bid_price: f64,
    bid_qnt: f64,
    ask_price: f64,
    ask_qnt: f64,
    transaction_time: u64,
}
#[allow(non_snake_case)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
struct AggTradeWS {
    E: i64,
    M: bool,
    T: i64,
    a: i64,
    //e: String,
    f: i64,
    l: i64,
    m: bool,
    p: f64,
    q: f64,
    //s: String,
}
//{'t': 1748736000000, 'T': 1751327999999, 's': 'TRUMPUSDT', 'i': '1M', 'f': 141621259, 'L': 142166556, 'o': '11.24000000', 'c': '11.19000000', 'h': '11.90000000', 'l': '10.94000000', 'v': '15876266.19100000', 'n': 545298, 'x': False, 'q': '179814040.65914000', 'V': '8111489.88500000', 'Q': '91972469.36091000', 'B': '0'}
#[allow(non_snake_case)]
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
pub struct KlineTick {
    t: i64, // open time
    T: i64, // close time
    //s: String, //symbol
    //i: String, //Interval
    f: i64, //??
    L: i64, // ??
    o: f64,
    c: f64,
    h: f64,
    l: f64,
    v: f64,
    n: i64,  //no of trades
    x: bool, //kline open or close
    q: f64,  //qout taer volume
    V: f64,
    Q: f64,
    B: i64, //ignore
}
impl KlineTick {
    pub fn to_kline_vec(input: &[KlineTick]) -> Vec<(DateTime<Utc>, f64, f64, f64, f64, f64)> {
        input
            .iter()
            .map(|i| {
                let open_time = DateTime::<Utc>::from_timestamp_millis(i.t).expect("Fukcckckck");
                (open_time, i.o, i.h, i.l, i.c, i.v)
            })
            .collect()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct TradeWS {}

#[allow(unused)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum BinWSResponse {
    OrderBook(OrderBookWS),
    AggTrade(AggTradeWS),
    Trade(TradeWS),
    KlineTick((Intv, KlineTick)),
}
impl Default for BinWSResponse {
    fn default() -> BinWSResponse {
        BinWSResponse::AggTrade(AggTradeWS::default())
    }
}

#[allow(unused)]
impl BinWSResponse {
    fn subscribe_key(&self, symbol: &str) -> String {
        let ret = match &self {
            BinWSResponse::OrderBook(_) => {
                format!["{}@bookTicker", symbol.to_lowercase()]
            }
            BinWSResponse::AggTrade(_) => {
                format!["{}@aggTrade", symbol.to_lowercase()]
            }
            BinWSResponse::Trade(_) => {
                format!["{}@Trade", symbol.to_lowercase()]
            }
            BinWSResponse::KlineTick((intv, _)) => {
                format!["{}@kline_{}", symbol.to_lowercase(), intv.to_bin_str()]
            }
        };
        tracing::debug!["{}", &ret];
        ret
    }
    fn parse_into_by_str(input: Value) -> Result<(String, BinWSResponse)> {
        let ws_output_type = input["e"]
            .as_str()
            .ok_or(anyhow!["E not found in binance output: {:}?", input])?;
        tracing::trace!["\x1b[93m Input Value \x1b[0m: {:?}", &input];
        let ret = match ws_output_type {
            "bookTicker" => {
                //BinWSResponse::OrderBook(_);
                todo!()
            }
            "aggTrade" => {
                let symbol = input
                    .get("s")
                    .ok_or(anyhow!["s Symbol not found in input :{:?}", input])?
                    .as_str()
                    .ok_or(anyhow!["Unable to parse s"])?
                    .to_string();

                #[cfg(debug_assertions)]
                let t_now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
                #[cfg(debug_assertions)]
                let event_time = input["E"]
                    .as_i64()
                    .ok_or(anyhow!["Unable to parse E as i64"])?;
                #[cfg(debug_assertions)]
                tracing::trace![
                    "\x1b[34m (AGGTrade Current linux epoch - event_time)\x1b[0m: {}ms",
                    t_now - event_time
                ];

                Ok((
                    symbol,
                    BinWSResponse::AggTrade(
                        //NOTE this code makes me want to get aids...
                        AggTradeWS {
                            E: input["E"].as_i64().ok_or(anyhow!["Unable to parse E"])?,
                            M: input["M"].as_bool().ok_or(anyhow!["Unable to parse M"])?,
                            T: input["T"].as_i64().ok_or(anyhow!["Unable to parse T"])?,
                            a: input["a"].as_i64().ok_or(anyhow!["Unable to parse a"])?,
                            f: input["f"].as_i64().ok_or(anyhow!["Unable to parse f"])?,
                            l: input["l"].as_i64().ok_or(anyhow!["Unable to parse l"])?,
                            p: input["p"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse p"])?
                                .parse()?,
                            q: input["q"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse q"])?
                                .parse()?,
                            m: input["m"].as_bool().ok_or(anyhow!["Unable to parse "])?,
                        },
                    ),
                ))
            }
            "kline" => {
                tracing::trace!["Parsing kline: {}", &input["k"]];
                let symbol = input
                    .get("s")
                    .ok_or(anyhow!["s Symbol not found in input :{:?}", input])?
                    .as_str()
                    .ok_or(anyhow!["Unable to parse s"])?
                    .to_string();
                tracing::trace!["Parsing kline 2: {}", &input["k"]];
                let interval = input["k"]
                    .get("i")
                    .ok_or(anyhow!["i Interval not found in input :{:?}", input])?
                    .as_str()
                    .ok_or(anyhow!["Unable to parse i"])?;

                let i = Intv::from_bin_str(interval);
                let input2 = &input["k"];

                #[cfg(debug_assertions)]
                let t_now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
                #[cfg(debug_assertions)]
                let event_time = input["E"]
                    .as_i64()
                    .ok_or(anyhow!["Unable to parse E as i64"])?;
                #[cfg(debug_assertions)]
                tracing::trace![
                    "\x1b[31m (KLINE Current linux epoch - event_time)\x1b[0m: {}ms",
                    t_now - event_time
                ];
                /*
                 */
                Ok((
                    symbol,
                    BinWSResponse::KlineTick((
                        i,
                        //NOTE this code makes me want to get aids...
                        KlineTick {
                            t: input2["t"].as_i64().ok_or(anyhow!["Unable to parse t"])?,
                            T: input2["T"].as_i64().ok_or(anyhow!["Unable to parse T"])?,
                            f: input2["f"].as_i64().ok_or(anyhow!["Unable to parse f"])?,
                            L: input2["L"].as_i64().ok_or(anyhow!["Unable to parse L"])?,
                            o: input2["o"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse o"])?
                                .parse()?,
                            h: input2["h"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse o"])?
                                .parse()?,
                            l: input2["l"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse o"])?
                                .parse()?,
                            c: input2["c"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse o"])?
                                .parse()?,
                            v: input2["v"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse v"])?
                                .parse()?,
                            n: input2["n"].as_i64().ok_or(anyhow!["Unable to parse n"])?,
                            x: input2["x"].as_bool().ok_or(anyhow!["Unable to parse x"])?,
                            q: input2["q"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse q"])?
                                .parse()?,
                            V: input2["V"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse V"])?
                                .parse()?,
                            Q: input2["Q"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse Q"])?
                                .parse()?,
                            B: input2["B"]
                                .as_str()
                                .ok_or(anyhow!["Unable to parse B"])?
                                .parse()?,
                        },
                    )),
                ))
            }
            _ => Err(anyhow!["Unable to parse ws output:{:?}", input]),
        };
        tracing::trace!["{:?}", &ret];
        ret
    }
}

#[derive(Debug, Default, Clone)]
pub struct SymbolOutput {
    trade: Vec<TradeWS>,
    agg_trade: Vec<AggTradeWS>,
    order_book: Vec<OrderBookWS>,
    pub closed_klines: HashMap<Intv, Vec<KlineTick>>,
    pub all_klines: HashMap<Intv, Vec<KlineTick>>,
}

#[derive(Debug, Default)]
pub struct WSTick {
    unsorted_output: Mutex<Vec<(String, BinWSResponse)>>,
    watch_price_ty: BinWSResponse,
    sub_params: HashMap<String, Vec<String>>,
    symbol_sorted_output: Arc<Mutex<HashMap<String, SymbolOutput>>>,
    live_price1: Arc<Mutex<f64>>,
    disconnect: bool,
}
impl WSTick {
    fn new(
        symbol_sorted_output: Arc<Mutex<HashMap<String, SymbolOutput>>>,
        live_price1: Arc<Mutex<f64>>,
    ) -> Self {
        Self {
            symbol_sorted_output,
            live_price1,
            ..Default::default()
        }
    }
    async fn sort(&mut self) -> Vec<(String, BinWSResponse)> {
        let mut uo = self
            .unsorted_output
            .lock()
            .expect("Unable to unlock mutex: binance::sort()");
        uo.drain(..).collect::<Vec<(String, BinWSResponse)>>()
    }
    async fn place_unsorted(&mut self, symbol: String, input: BinWSResponse) -> Result<()> {
        tracing::trace!["Binws response: {:?}", input];
        match input {
            BinWSResponse::OrderBook(o) => {
                let mut cum_queue = self
                    .symbol_sorted_output
                    .lock()
                    .expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(queue) = cum_queue.get_mut(&symbol) {
                    queue.order_book.push(o);
                } else {
                    let mut queue = SymbolOutput::default();
                    queue.order_book.push(o);
                    cum_queue.insert(symbol, queue);
                };
            }
            BinWSResponse::AggTrade(o) => {
                let mut cum_queue = self
                    .symbol_sorted_output
                    .lock()
                    .expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(queue) = cum_queue.get_mut(&symbol) {
                    queue.agg_trade.push(o);
                    tracing::trace!["Agg trade queue {:?}", &queue];
                } else {
                    let mut queue = SymbolOutput::default();
                    queue.agg_trade.push(o);
                    cum_queue.insert(symbol, queue);
                };
            }
            BinWSResponse::Trade(o) => {
                let mut cum_queue = self
                    .symbol_sorted_output
                    .lock()
                    .expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(queue) = cum_queue.get_mut(&symbol) {
                    queue.trade.push(o);
                } else {
                    let mut queue = SymbolOutput::default();
                    queue.trade.push(o);
                    cum_queue.insert(symbol, queue);
                };
            }
            BinWSResponse::KlineTick((intv, kline)) => {
                let mut cum_queue = self
                    .symbol_sorted_output
                    .lock()
                    .expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(queue) = cum_queue.get_mut(&symbol) {
                    if kline.x == true {
                        tracing::trace!["\x1b  CLOSED kline \x1b[93m  = {:?}", &kline];
                        if let Some(kline_closed_queue) = queue.closed_klines.get_mut(&intv) {
                            kline_closed_queue.push(kline.clone());
                            //clear the tick queue once the kline is closed
                            tracing::trace![
                                "\x1b kline_tick CLOSED queue size \x1b[93m  = {:?}",
                                kline_closed_queue
                            ];
                            queue.all_klines.insert(intv, vec![]);
                            if let Some(kline_all_queue) = queue.all_klines.get_mut(&intv) {
                                kline_all_queue.clear();
                                tracing::trace!["\x1b kline_tick OPEN clear() ran! \x1b[93m  "];
                            };
                            //Notify that a kline is closed - close time
                        } else {
                            queue.closed_klines.insert(intv, vec![kline.clone()]);
                            tracing::trace![
                                "\x1b kline_tick CLOSED queue size :1 - initiated\x1b[93m"
                            ];
                            //plot closed_klines directly and fill any gaps
                        };
                    } else {
                        if let Some(kline_all_queue) = queue.all_klines.get_mut(&intv) {
                            kline_all_queue.push(kline.clone());
                            tracing::trace![
                                "\x1b kline_tick OPEN queue size \x1b[93m  = {:?}",
                                &kline_all_queue.len()
                            ];
                        } else {
                            queue.all_klines.insert(intv, vec![kline.clone()]);
                            tracing::trace![
                                "\x1b kline_tick OPEN queue size :1 - initiated\x1b[93m"
                            ];
                        };
                    };
                } else {
                    let mut queue = SymbolOutput::default();
                    if kline.x == true {
                        queue.closed_klines.insert(intv, vec![kline.clone()]);
                    } else {
                        queue.all_klines.insert(intv, vec![kline.clone()]);
                    };
                    cum_queue.insert(symbol, queue);
                };
            }
        }
        Ok(())
    }
    async fn append_tick(&mut self, input: Value) -> Result<()> {
        tracing::trace!["\x1b[93m Raw ws output\x1b[93m : {:?}", input];
        let (symbol, sorted) =
            BinWSResponse::parse_into_by_str(input).context("Failed to parse into str")?;
        match (sorted, self.watch_price_ty) {
            (BinWSResponse::Trade(_val), BinWSResponse::Trade(_)) => {
                //todo!()
            }
            (BinWSResponse::AggTrade(val), BinWSResponse::AggTrade(_)) => {
                let mut lp1 = self
                    .live_price1
                    .lock()
                    .expect("Unable to unlock mutex: binance::append_tick()");
                *lp1 = val.p;
            }
            (BinWSResponse::OrderBook(_val), BinWSResponse::AggTrade(_)) => {
                //todo!()
            }
            (BinWSResponse::KlineTick((_, _val)), BinWSResponse::KlineTick((_, _))) => {
                //todo!()
            }
            _ => (),
        }
        let mut uo = self
            .unsorted_output
            .lock()
            .expect("Unable to unlock mutex: binance::append_tick()");
        tracing::trace!["Placing ws output"];
        uo.push((symbol, sorted));
        tracing::trace!["Raw ws successfully placed"];
        Ok(())
    }
    async fn connect(&self) -> Result<WebSocket> {
        let mut p = self.sub_params.clone();
        tracing::debug!["\x1b[93m Subscribe message hashmap\x1b[93m : {:?}", p];
        let mut ovec: Vec<String> = vec![];
        let _ = p
            .iter_mut()
            .map(|(_, mut out)| {
                tracing::debug!["\x1b[93m Iter out\x1b[93m : {:?}", out];
                ovec.append(&mut out);
            })
            .collect::<()>();
        //let params = ["btcustd@Trade","btcustd@aggTrade", "btcusdt@bookTicker"];
        tracing::trace!["\x1b[93m Ovec \x1b[0m: {:?}", ovec];
        let socket = "wss://stream.binance.com:9443/ws";
        let sub_message = json!({
            "method": "SUBSCRIBE",
            "id": 1,
            "params":ovec.as_slice(), //NOTE press x to doubt...
        });
        tracing::debug!["\x1b[93m Subscribe message\x1b[0m :  {:?}", sub_message];
        let mut conn = WebSocket::connect(socket).await?;
        conn.send_text(sub_message.to_string()).await?;
        tracing::info!["Web socket connected with message:{:?}", sub_message];
        Ok(conn)
    }
    async fn collect_output(&mut self, buffer_size: usize, conn: &mut WebSocket) -> Result<()> {
        let mut n = 0;
        loop {
            while n < buffer_size {
                let msg = conn.receive().await?;
                tracing::trace!["\x1b[93m WS raw output: \x1b[0m {:?}", msg];
                if let Some((m, _, _)) = msg.clone().into_text() {
                    let v: Value = serde_json::from_str(&m)?;
                    if let Some(_) = v.get("id") {
                        tracing::trace!["\x1b[93m WS output (NOT APPEND): \x1b[0m {:?}", msg];
                    } else {
                        tracing::trace!["\x1b[93m WS output (APPEND): \x1b[0m {:?}", msg];
                        self.append_tick(v).await.context("Failed to append tick")?;
                        n += 1;
                    }
                }
            }
            let mut buffer = self.sort().await;
            while let Some((symbol, response)) = buffer.pop() {
                tracing::trace!["\x1b[93m WS placing raw output: \x1b[0m {:?}", response];
                self.place_unsorted(symbol, response)
                    .await
                    .context("Unable to sort WS output")?;
            }
            n = 0;
            tracing::trace!["\x1b[93m Unsorted  messages placed \x1b[0m"];
            //TODO start multiple websockets for multiple symbols?
            if self.disconnect == true {
                tracing::debug!["\x1b[93m WS disconnect called!\x1b[0m"];
                break;
            }
        }
        conn.close(None).await?;
        self.disconnect = false;
        Ok(())
    }
    async fn run(&mut self, buffer_size: usize) -> Result<()> {
        let mut conn = self.connect().await?;
        self.collect_output(buffer_size, &mut conn).await?;
        //TODO delete output once Kline is closed, send it to AD
        Ok(())
    }
}

#[derive(Dbg)]
pub struct BinanceClient {
    #[dbg(skip)]
    pub binance_client: Binance,
    pub pub_key: Option<String>,
    pub sec_key: Option<String>,
    pub current_order_id: u64,
    pub ws_buffer_size: usize,
    pub ws_tick: WSTick,

    pub api_keys_valid: bool,
    pub account_info: Option<AccountInformation>,
    pub exchange_info: Vec<SymbolInfo>,
    pub live_ad: Arc<Mutex<AssetData>>,
    pub live_info: Arc<Mutex<LiveInfo>>,

    pub current_symbol: String,
    pub base_balances: (f64, f64),
    pub qoute_balances: (f64, f64),
    pub balances: HashMap<String, (f64, f64)>,
    pub current_symbol_bases: (String, String),
    pub live_orders: HashMap<i32, (Order, bool)>,
    pub stop_client:bool,
    pub ws_connect:bool,

    pub default_symbol: String,
    pub default_intv: Intv,
}

impl Default for BinanceClient {
    fn default() -> Self {
        Self {
            binance_client: Binance::default(),
            pub_key: None,
            sec_key: None,
            current_order_id: 0,
            ws_buffer_size: 50,
            ws_tick: WSTick::default(),
            account_info: None,
            exchange_info: vec![],
            live_ad: Arc::new(Mutex::new(AssetData::default())),
            live_info: Arc::new(Mutex::new(LiveInfo::default())),
            api_keys_valid: false,
            stop_client:false,
            ws_connect:false,

            current_symbol: String::default(),
            default_symbol: String::default(),
            default_intv: Intv::default(),
            base_balances: (0.0, 0.0),
            qoute_balances: (0.0, 0.0),
            balances: HashMap::new(),
            live_orders: HashMap::new(),
            current_symbol_bases: (String::default(), String::default()),
        }
    }
}

impl BinanceClient {
    pub fn new(
        pub_key: Option<String>,
        sec_key: Option<String>,
        collect: Arc<Mutex<HashMap<String, SymbolOutput>>>,
        live_price_watch: Arc<Mutex<f64>>,
        live_ad: Arc<Mutex<AssetData>>,
        live_info: Arc<Mutex<LiveInfo>>,
    ) -> Self {
        Self {
            binance_client: Binance::default(),
            pub_key,
            sec_key,
            current_order_id: 0,
            ws_buffer_size: 15,
            ws_tick: WSTick::new(collect, live_price_watch),
            account_info: None,
            exchange_info: vec![],
            live_ad,
            live_info,
            ..Default::default()
        }
    }
    async fn remove_api_keys(&mut self) -> Result<()> {
        self.binance_client = Binance::default();
        Ok(())
    }
    async fn add_replace_api_keys(&mut self, pub_key: &str, priv_key: &str) -> Result<()> {
        self.binance_client = Binance::with_key_and_secret(&pub_key, &priv_key);
        let res = self.get_all_balances().await;
        //FIXME unlock the mutex twice here, fix later
        let live_inf = self.live_info.clone();
        let mut live_info = live_inf.lock().expect("live_info poisoned mutex");
        match res {
            Ok(_) => {
                tracing::info!["API keys changed, account balances updated"];
                self.api_keys_valid = true;
                live_info.keys_status = KeysStatus::Valid;
            }
            Err(e) => {
                tracing::error!["Unable to get binance account balances: {}", e];
                self.api_keys_valid = false;
                live_info.keys_status = KeysStatus::Invalid;
            }
        };
        Ok(())
    }
    async fn send_new_order(
        &self,
        sym: &str,
        otype: OrderType,
        sid: OrderSide,
        pric: f64,
        quant: f64,
    ) -> Result<u64> {
        let resp = self
            .binance_client
            .request(usdm::NewOrderRequest {
                symbol: sym.into(),
                //r#type: OrderType::Limit,
                r#type: otype,
                //side: OrderSide::Buy,
                side: sid,
                price: Decimal::from_f64(pric),
                quantity: Decimal::from_f64(quant),
                time_in_force: Some(TimeInForce::GTC),
                ..Default::default()
            })
            .await
            .context(ERR_CTX)?;
        tracing::info!("{:?}", resp);
        Ok(resp.order_id)
    }
    async fn cancel_order(&self, sym: &str, id: &i32) -> Result<()> {
        let resp = self
            .binance_client
            .request(usdm::CancelOrderRequest {
                symbol: sym.into(),
                order_id: Some(*id as u64),
                ..Default::default()
            })
            .await
            .context(ERR_CTX)?;
        tracing::info!("{:?}", resp);
        Ok(())
    }
    async fn cancel_all_orders(&self, symbol: &str, ids: Vec<u64>) -> Result<()> {
        let s = symbol.to_string();
        let resp = self
            .binance_client
            .request(usdm::CancelMultipleOrdersRequest {
                symbol: s,
                order_id_list: ids,
                ..Default::default()
            })
            .await
            .context(ERR_CTX)?;
        tracing::info!("{:?}", resp);
        Ok(())
    }
    #[allow(unused)]
    async fn start_user_stream(&self) -> Result<()> {
        let listen_key = self
            .binance_client
            .request(StartUserDataStreamRequest {})
            .await
            .context(ERR_CTX)
            .context("Binance client:unable to connect_ws user stream")?;
        let mut ws: BinanceWebsocket<WebsocketMessage> =
            BinanceWebsocket::new(&[listen_key.listen_key.as_str()])
                .await
                .context(ERR_CTX)?;
        loop {
            let msg = ws
                .next()
                .await
                .ok_or(anyhow!["Ws exited on a None"])
                .context(ERR_CTX)?;
            tracing::debug!("{msg:?}");
        }
        Ok(())
    }
    pub async fn connect_ws(&mut self, params: HashMap<String, Vec<String>>) -> Result<()> {
        //NOTE may have to get rid this self due to inter mutability,,,
        self.exchange_info = get_exchange_info().await?;
        self.ws_tick.sub_params = params;
        self.ws_tick
            .run(self.ws_buffer_size)
            .await
            .context(ERR_CTX)?;
        tracing::trace!["Connect WS ws: {:?}", &self];
        Ok(())
    }
    #[allow(unused)]
    pub async fn get_initial_data(
        &mut self,
        symbol: &str,
        default_intv: &Intv,
        limit: usize,
        live_ad: Arc<Mutex<AssetData>>,
    ) -> Result<()> {
        tracing::trace!["Get init client called!"];

        let res = validate_asset_binance(symbol).await?;
        match res {
            true => (),
            false => {
                tracing::error![
                    "get_initial_data: unable to find asset {} on Binance",
                    &symbol
                ];
                return Ok(());
            }
        };
        let ss=symbol.to_string();
        let mut klines_unordered=Intv::iter().map(move |i| {
            let s=ss.clone();
            let intv=i.clone();
            let client = reqwest::Client::new();
            tokio::task::spawn(async move{
                get_latest_wicks2(&client, &s, &intv).await
            })
        }).collect::<FuturesUnordered<_>>();
        let mut ku=pin![klines_unordered];

        let mut klines = Klines::new_empty();
        while let Some(res) = ku.next().await{
            match res{
                Ok((kline,intv))=>{
                    klines.insert(&intv,kline);
                    tracing::debug!["kline inserted for Symbol:{} Interval:{:?}",&symbol,intv];
                }
                Err(e)=>{
                    tracing::error!["Kline JoinError:{}",e];
                }
            }
        };

        //FIXME
        if self.api_keys_valid == true {
            self.get_balances(&symbol);
        };

        let mut live_b = live_ad
            .lock()
            .expect("Poisoned live AD mutex at get_initial_data");

        live_b.kline_data.insert(symbol.to_string(), klines);
        live_b.acc_balances = self.balances.clone();
        live_b.current_pair_strings = self.current_symbol_bases.clone();
        live_b.current_pair_free_balances = (self.base_balances.0, self.qoute_balances.1);
        live_b.current_pair_locked_balances = (self.base_balances.1, self.qoute_balances.0);

        Ok(())
    }
    async fn get_balances(&mut self, symbol: &str) -> Result<()> {
        let res = self.binance_client.request(GetAccountRequest {}).await;
        let mut balances: HashMap<String, (f64, f64)> = HashMap::new();
        match res {
            Ok(acc) => {
                let _: Vec<_> = acc
                    .balances
                    .iter()
                    .map(|a| {
                        let free = match a.free.to_f64() {
                            Some(res) => res,
                            None => 0.0,
                        };
                        let locked = match a.locked.to_f64() {
                            Some(res) => res,
                            None => 0.0,
                        };
                        balances.insert(a.asset.clone(), (free, locked))
                    })
                    .collect();
            }
            Err(e) => {
                tracing::error!["Get balances error: {}", e];
            }
        };
        let res = get_asset_bases_binance(&symbol).await?;
        match res {
            Some((base, qoute)) => {
                let (base_free, base_locked) = balances
                    .get(&base)
                    .ok_or(anyhow!["Base asset: {} not found in balances!", &base])?;
                let (qoute_free, qoute_locked) = balances
                    .get(&qoute)
                    .ok_or(anyhow!["Base asset: {} not found in balances!", &qoute])?;
                self.current_symbol = symbol.to_string();
                self.base_balances = (*base_free, *base_locked);
                self.qoute_balances = (*qoute_free, *qoute_locked);
                self.balances = balances;
                self.current_symbol_bases = (base, qoute);
            }
            None => (),
        };
        Ok(())
    }
    async fn get_all_balances(&mut self) -> Result<()> {
        let res = self.binance_client.request(GetAccountRequest {}).await;
        let mut balances: HashMap<String, (f64, f64)> = HashMap::new();
        match res {
            Ok(acc) => {
                let _: Vec<_> = acc
                    .balances
                    .iter()
                    .map(|a| {
                        let free = match a.free.to_f64() {
                            Some(res) => res,
                            None => 0.0,
                        };
                        let locked = match a.locked.to_f64() {
                            Some(res) => res,
                            None => 0.0,
                        };
                        balances.insert(a.asset.clone(), (free, locked))
                    })
                    .collect();
            }
            Err(e) => {
                tracing::error!["Get balances error: {}", e];
            }
        };
        let live_inf = self.live_info.clone();
        let mut live_info = live_inf
            .lock()
            .expect("Unable to unlock live_info mutex: bin_client side");
        live_info.acc_balances = balances;
        Ok(())
    }
    #[allow(unused)]
    async fn change_ws_params(&mut self, new_params: HashMap<String, Vec<String>>) -> Result<()> {
        //TODO check if possible to change params without disconnecting
        self.ws_tick.disconnect = true;
        self.ws_tick.sub_params = new_params;
        self.ws_tick
            .run(self.ws_buffer_size)
            .await
            .context(ERR_CTX)?;
        Ok(())
    }
    pub async fn disconnect_ws(&mut self) {
        self.ws_tick.disconnect = true;
    }
    pub async fn get_user_data(&mut self) -> Result<()> {
        let resp = self
            .binance_client
            .request(GetAccountRequest {})
            .await
            .context(ERR_CTX)?;
        tracing::debug!("{:?}", resp);
        self.account_info = Some(resp);
        Ok(())
    }
    pub async fn get_def_ws_params(
        &mut self,
        get_initial_data:bool,
    )->Result<HashMap<String, Vec<String>>>{
        let def_sym=self.default_symbol.clone();
        let def_intv=self.default_intv;
        let p=self.get_ws_params(&def_sym,&def_intv, get_initial_data).await?;
        Ok(p)
    }
    pub async fn get_curr_ws_params(
        &mut self,
        get_initial_data:bool,
    )->Result<HashMap<String, Vec<String>>>{
        let def_sym=self.current_symbol.clone();
        let def_intv=self.default_intv;
        let p=self.get_ws_params(&def_sym,&def_intv, get_initial_data).await?;
        Ok(p)
    }
    pub async fn get_ws_params(
        &mut self,
        symbol: &str,
        default_intv: &Intv,
        get_initial_data:bool,
    ) -> Result<HashMap<String, Vec<String>>> {
        let mut sub_params = vec![format!["{}@aggTrade", symbol.to_lowercase()]];
        for i in Intv::iter() {
            sub_params.push(format![
                "{}@kline_{}",
                symbol.to_lowercase(),
                i.to_bin_str()
            ])
        }
        let mut params: HashMap<String, Vec<String>> = HashMap::new();
        params.insert(symbol.to_string(), sub_params);

        if get_initial_data{
            let res = self
                .get_initial_data(&symbol, &default_intv, 2_000, self.live_ad.clone())
                .await;
            match res {
                Ok(_) => {
                    tracing::trace!["Initial data for {} received",&symbol];
                    let mut ad = self
                        .live_ad
                        .lock()
                        .expect("get_ws_params: Unable to unlock mutex");
                    ad.live_asset_symbol_changed = (true, symbol.to_string());
                }
                Err(e) => tracing::error!["Initial data connection failed, ERROR: {}", e],
            };

        };

        Ok(params)
    }
    pub fn update_settings(&mut self, _settings: &Settings) -> Result<()> {
        //TODO
        Ok(())
    }
    pub async fn parse_binance_instructs(&mut self, i: BinInstructs) -> BinResponse {
        match i {
            BinInstructs::CancelOrder { id, symbol, .. } => {
                let res = self.cancel_order(&symbol, &id).await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow!["Unable to cancel_order:{}", e.context(ERR_CTX)]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::UpdateSettings(ref settings) => {
                let res = self.update_settings(&settings);
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow!["Unable to update_settings:{}", e.context(ERR_CTX)]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::RemoveApiKeys => {
                let res = self.remove_api_keys().await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow![
                                "{:?} Unable to connect to ws  e:{}",
                                i.clone(),
                                e.context(ERR_CTX)
                            ]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::GetAllBalances => {
                let res = self.get_all_balances().await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow![
                                "{:?} Unable to connect to ws  e:{}",
                                i.clone(),
                                e.context(ERR_CTX)
                            ]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::GetBalance { ref symbol } => {
                let res = self.get_balances(symbol).await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow![
                                "{:?} Unable to connect to ws  e:{}",
                                i.clone(),
                                e.context(ERR_CTX)
                            ]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::AddReplaceApiKeys {
                ref pub_key,
                ref priv_key,
            } => {
                let res = self.add_replace_api_keys(pub_key, priv_key).await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow![
                                "{:?} Unable to connect to ws  e:{}",
                                i.clone(),
                                e.context(ERR_CTX)
                            ]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::ConnectWS { params: ref p } => {
                tracing::trace!["Connect ws start {:?}", &self];
                let res = self.connect_ws(p.clone()).await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow![
                                "{:?} Unable to connect to ws  e:{}",
                                i.clone(),
                                e.context(ERR_CTX)
                            ]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::ConnectUserWS { params: _ } => {
                tracing::debug!["Connect ws start {:?}", &self];
                let res = self.start_user_stream().await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(ERR_CTX)]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::Disconnect => {
                let _res = self.disconnect_ws().await;
                BinResponse::Success
            }
            BinInstructs::GetUserData => {
                tracing::debug!["Connect ws start {:?}", &self];
                let res = self.get_user_data().await;
                let resp = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!("{}", anyhow!["{:?} Unable to  e:{}", i, e]);
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::PlaceOrder {
                symbol: ref s,
                o: order,
            } => {
                tracing::debug!["Connect ws start {:?}", &self];
                let (order_type, order_side, price, _stop_price, quantity) =
                    parse_order_to_ba(&order);
                let res = self
                    .send_new_order(&s, order_type, order_side, price, quantity)
                    .await;
                let resp = match res {
                    Ok(order_id) => {
                        let live_inf = self.live_info.clone();
                        let mut live_i = live_inf.lock().expect("Unable to unlock live_info mutex");
                        live_i.live_orders.insert(order_id as i32, (order, true));
                        self.live_orders = live_i.live_orders.clone();
                        BinResponse::Success
                    }
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(ERR_CTX)]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::CancelAndReplaceOrder {
                id,
                symbol: ref s,
                o: order,
            } => {
                tracing::debug!["Connect ws start {:?}", &self];
                let res = self.cancel_order(&s, &id).await;
                match res {
                    Ok(_) => {}
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(ERR_CTX)]
                        );
                        return BinResponse::Failure((string_error, GeneralError::Generic));
                    }
                };
                let (order_type, order_side, price, _stop_price, quantity) =
                    parse_order_to_ba(&order);
                let res = self
                    .send_new_order(&s, order_type, order_side, price, quantity)
                    .await;
                let resp = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(ERR_CTX)]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::CancelAllOrders { symbol: ref s } => {
                let o_ids: Vec<u64> = self
                    .live_orders
                    .iter()
                    .map(|(id, (_order, _active))| *id as u64)
                    .collect();
                let res = self.cancel_all_orders(&s, o_ids).await;
                let resp = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!(
                            "{}",
                            anyhow![
                                "{:?} Unable to sell_now e:{}",
                                i.clone(),
                                e.context(ERR_CTX)
                            ]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::ChangeLiveAsset {
                symbol: ref s,
                defualt_symbol: ref ss,
            } => {
                let res=validate_asset_binance(s).await;
                match res{
                    Ok(ans)=>{
                        match ans{
                            true=>(),
                            false=>{
                                tracing::error!["Cannot find asset on binance:{}",s];
                                return BinResponse::Failure((format!["Cannot find asset on binance:{}",s], GeneralError::Generic));
                            }
                        }
                    },
                    Err(e)=>{
                        tracing::error!["Validate binance asset error:{}",e];
                        return BinResponse::Failure((format!["{}",e], GeneralError::Generic));
                    }
                };
                let res = self.get_ws_params(s, &Intv::default(), false).await;
                let params = match res {
                    Ok(params) => params,
                    Err(e) => {
                        let string_error = format!["{}", &e];
                        tracing::error!(
                            "{}",
                            anyhow![
                                "Unable to change asset: {}, connecting with default parameters",
                                string_error
                            ]
                        );

                        let symbol = ss;
                        let mut sub_params = vec![format!["{}@aggTrade", symbol.to_lowercase()]];
                        for i in Intv::iter() {
                            sub_params.push(format![
                                "{}@kline_{}",
                                symbol.to_lowercase(),
                                i.to_bin_str()
                            ])
                        }
                        let mut params: HashMap<String, Vec<String>> = HashMap::new();
                        params.insert(symbol.to_string(), sub_params);
                        params
                    }
                };
                self.ws_tick.sub_params=params.clone();
                self.current_symbol=s.clone();
                tracing::debug!["Live re-connect params {:?}",params];
                self.disconnect_ws().await;
                BinResponse::Success
            }
            BinInstructs::None => BinResponse::None,
        }
    }
}

fn parse_order_to_ba(order: &Order) -> (OrderType, OrderSide, f64, f64, f64) {
    //async fn send_new_order(&self, sym:String, otype: OrderType, sid:OrderSide, pric:f64, quant:f64)
    tracing::debug!["Order to parse:{:?}", &order];
    let res: (OrderType, bool, f64, f64, f64) = match order {
        Order::Market { buy: b, quant: q } => (OrderType::Market, *b, 0.0, 0.0, q.get_f64()),
        Order::Limit {
            buy: b,
            quant: q,
            price: p,
            limit_status: _,
        } => (OrderType::Limit, *b, *p, 0.0, q.get_f64()),
        Order::StopLimit {
            buy: b,
            quant: q,
            price: p,
            limit_status: _,
            stop_price: sp,
            stop_status: _,
        } => (OrderType::StopLossLimit, *b, *p, *sp as f64, q.get_f64()),
        Order::StopMarket {
            buy: b,
            quant: q,
            price: p,
            stop_status: _,
        } => (OrderType::StopLoss, *b, *p, 0.0, q.get_f64()),
        Order::None => {
            panic!("Order none should never be parsed!")
        }
    };
    let side;
    if res.1 == true {
        side = OrderSide::Buy;
    } else {
        side = OrderSide::Sell;
    }
    tracing::debug!["End order parsed:{:?}", &res];
    (res.0, side, res.2, res.3, res.4)
}
#[allow(unused)]
#[derive(Deserialize, Debug)]
#[allow(non_snake_case)]
pub struct AggTrade {
    E: i64,
    M: bool,
    T: i64,
    a: i64,
    e: String,
    f: i64,
    l: i64,
    m: bool,
    p: f64,
    q: f64,
    s: String,
}
//TODO - fix this derive

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    //TODO  make binance api tests
    async fn example() {}
}
