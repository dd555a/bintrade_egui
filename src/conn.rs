use binance::account::*;
use binance::api::Binance;
use binance::rest_model::{
    AccountInformation, Order as BinanceOrder, OrderSide, OrderStatus, OrderType, TimeInForce,
};
use tokio::time::{Duration, sleep};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[cfg(debug_assertions)]
use std::time::{SystemTime, UNIX_EPOCH};

use strum::IntoEnumIterator;

use serde::Deserialize;
use serde_json::Value;

use anyhow::{Context, Result, anyhow};
use derive_debug::Dbg;
use futures::StreamExt;

use num::ToPrimitive;
use reqwest;
use serde_json::json;
use websockets::WebSocket;

use core::pin::pin;
use futures::stream::FuturesUnordered;

use crate::data::{
    AssetData, Intv, Kline as KlineMine, Klines, get_asset_bases_binance, validate_asset_binance,
};
use crate::gui::{KeysStatus, LiveInfo, Settings};
use crate::trade::{LimitStatus, Order, Quant, StopStatus};
use crate::{BinInstructs, BinResponse, GeneralError};

use chrono::{DateTime, Utc};

const ERR_CTX: &str = "Binance client | websocket:";
const DEFAULT_BUFFER_SIZE: usize = 50;
const ORDER_CHECK_INTV_MS: u64 = 500;
//TODO api needs to be replaced, start ws user stream needs to be done, and it needs to be
//refactored for easier debuging

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
async fn get_latest_wicks2(client: &reqwest::Client, symbol: &str, i: &Intv) -> (KlineMine, Intv) {
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
    (GetKline::to_kline(&kl), *i)
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
        //tracing::trace!["GET response:{}", &res];
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

#[allow(unused)]
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum BinWSResponse {
    AggTrade(AggTradeWS),
    KlineTick((Intv, KlineTick)),
}
impl Default for BinWSResponse {
    fn default() -> BinWSResponse {
        BinWSResponse::AggTrade(AggTradeWS::default())
    }
}

impl BinWSResponse {
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
    agg_trade: Vec<AggTradeWS>,
    pub closed_klines: HashMap<Intv, Vec<KlineTick>>,
    pub all_klines: HashMap<Intv, Vec<KlineTick>>,
}

#[derive(Debug, Default, Clone)]
pub struct WSTick {
    unsorted_output: Arc<
        Mutex<(
            Vec<(String, BinWSResponse)>,
            bool,
            bool,
            HashMap<String, Vec<String>>,
            Option<String>,
        )>,
    >,
    watch_price_ty: BinWSResponse,
    pub sub_params: HashMap<String, Vec<String>>,
    symbol_sorted_output: Arc<Mutex<HashMap<String, SymbolOutput>>>,
    live_price1: Arc<Mutex<f64>>,
    disconnect: bool,
    reconnect: bool,
    change_symbol: Option<String>,
    symbol: String,
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
        uo.0.drain(..).collect::<Vec<(String, BinWSResponse)>>()
    }
    async fn place_unsorted(&mut self, symbol: String, input: BinWSResponse) -> Result<()> {
        tracing::trace!["Binws response: {:?}", input];
        match input {
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
    async fn append_tick(&mut self, input: Value) -> Result<(bool, bool, Option<String>)> {
        tracing::trace!["\x1b[93m Raw ws output\x1b[93m : {:?}", input];
        let (symbol, sorted) =
            BinWSResponse::parse_into_by_str(input).context("Failed to parse into str")?;
        match (sorted, self.watch_price_ty) {
            (BinWSResponse::AggTrade(val), BinWSResponse::AggTrade(_)) => {
                if self.symbol == symbol || self.symbol.is_empty() {
                    let mut lp1 = self
                        .live_price1
                        .lock()
                        .expect("Unable to unlock mutex: binance::append_tick()");
                    *lp1 = val.p;
                };
            }
            (BinWSResponse::KlineTick((_, val)), BinWSResponse::KlineTick((_, _))) => {
                if self.symbol == symbol || self.symbol.is_empty() {
                    let mut lp1 = self
                        .live_price1
                        .lock()
                        .expect("Unable to unlock mutex: binance::append_tick()");
                    *lp1 = val.c;
                };
            }
            _ => (),
        }
        let mut uo = self
            .unsorted_output
            .lock()
            .expect("Unable to unlock mutex: binance::append_tick()");
        tracing::trace!["Placing ws output"];
        uo.0.push((symbol, sorted));
        tracing::trace!["Raw ws successfully placed"];
        let change_symbol = if uo.4.is_some() { uo.4.take() } else { None };
        Ok((uo.1, uo.2, change_symbol))
    }
    async fn connect(&self) -> Result<WebSocket> {
        let mut p = self.sub_params.clone();
        tracing::trace!["\x1b[93m Subscribe message hashmap\x1b[93m : {:?}", p];
        let mut ovec: Vec<String> = vec![];
        let _ = p
            .iter_mut()
            .map(|(_, mut out)| {
                tracing::trace!["\x1b[93m Iter out\x1b[93m : {:?}", out];
                ovec.append(&mut out);
            })
            .collect::<()>();
        //let params = ["btcustd@Trade","btcustd@aggTrade", "btcusdt@bookTicker"];
        tracing::trace!["\x1b[93m Ovec \x1b[0m: {:?}", ovec];
        let socket = "wss://stream.binance.com:9443/ws";
        let sub_message = json!({
            "method": "SUBSCRIBE",
            "id": 1,
            "params":ovec.as_slice(),
        });
        tracing::trace!["\x1b[93m Subscribe message\x1b[0m :  {:?}", sub_message];
        let mut conn = WebSocket::connect(socket).await?;
        conn.send_text(sub_message.to_string()).await?;
        tracing::trace!["Web socket connected with message:{:?}", sub_message];
        Ok(conn)
    }
    //NOTE this is horrible... refactor after changing API again
    async fn collect_output(&mut self, buffer_size: usize, conn: &mut WebSocket) -> Result<()> {
        let mut n = 0;
        tracing::trace![
            "collect_output started, self.disconnect {}",
            self.disconnect
        ];
        while !self.disconnect {
            while n < buffer_size {
                if let Some(ref new_symbol) = self.change_symbol {
                    tracing::debug!["Change symbol 2 called!"];
                    self.symbol = new_symbol.to_string();
                    let new_params = get_ws_params2(new_symbol);
                    let mut h = HashMap::default();
                    h.insert(new_symbol.to_string(), new_params.clone());
                    self.sub_params = h;
                    let sub_message = json!({
                        "method": "SUBSCRIBE",
                        "id": 1,
                        "params":new_params.as_slice(),
                    });
                    conn.send_text(sub_message.to_string()).await?;
                };
                let msg = conn.receive().await?;
                tracing::trace!["\x1b[93m WS raw output: \x1b[0m {:?}", msg];
                if let Some((m, _, _)) = msg.clone().into_text() {
                    let v: Value = serde_json::from_str(&m)?;
                    if let Some(_) = v.get("id") {
                        tracing::trace!["\x1b[93m WS output (NOT APPEND): \x1b[0m {:?}", msg];
                    } else {
                        tracing::trace!["\x1b[93m WS output (APPEND): \x1b[0m {:?}", msg];
                        (self.disconnect, self.reconnect, self.change_symbol) =
                            self.append_tick(v).await.context("Failed to append tick")?;
                        n += 1;
                    }
                }
            }
            let mut buffer = self.sort().await;
            while let Some((symbol, response)) = buffer.pop() {
                tracing::trace!["\x1b[93m WS placing raw output: \x1b[0m {:?}", response];
                let _res = self
                    .place_unsorted(symbol, response)
                    .await
                    .context("Unable to sort WS output")?;
            }
            n = 0;
            tracing::trace!["\x1b[93m Unsorted  messages placed \x1b[0m"];
        }
        conn.close(None).await?;
        tracing::trace!["\x1b[93m WS exited!!\x1b[0m"];
        Ok(())
    }
    async fn run(&mut self, buffer_size: usize) -> Result<()> {
        tracing::trace!["ws_tick.run started,  self.reconect: {}", self.reconnect];
        self.reconnect = true;
        loop {
            while self.reconnect {
                tracing::trace!["sub_params inloop :{:?}", self.sub_params];
                let mut conn = self.connect().await?;
                self.collect_output(buffer_size, &mut conn).await?;
            }
            sleep(Duration::from_millis(100)).await;
            let w = self
                .unsorted_output
                .lock()
                .expect("Unable to unlock unsorted output");
            self.reconnect = w.2;
            self.disconnect = w.1;
            self.sub_params = w.3.clone();
            tracing::trace!["ws_tick.run  INLOOP,  self.reconnect: {}", self.reconnect];
        }
    }
}

#[derive(Dbg)]
pub struct BinanceClient {
    #[dbg(skip)]
    pub binance_client: Account,
    pub current_order_id: u64,
    pub ws_buffer_size: usize,
    pub ws_tick: Option<WSTick>,

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
    pub live_orders: HashMap<u64, (Order, bool, f64)>,
    pub stop_client: bool,
    pub ws_connect: bool,

    pub default_symbol: String,
    pub default_intv: Intv,
}

impl Default for BinanceClient {
    fn default() -> Self {
        Self {
            binance_client: Account::new(None, None),
            current_order_id: 0,
            ws_buffer_size: 50,
            ws_tick: Some(WSTick::default()),
            account_info: None,
            exchange_info: vec![],
            live_ad: Arc::new(Mutex::new(AssetData::default())),
            live_info: Arc::new(Mutex::new(LiveInfo::default())),
            api_keys_valid: false,
            stop_client: false,
            ws_connect: false,

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
        config: &binance::config::Config,
        collect: Arc<Mutex<HashMap<String, SymbolOutput>>>,
        live_price_watch: Arc<Mutex<f64>>,
        live_ad: Arc<Mutex<AssetData>>,
        live_info: Arc<Mutex<LiveInfo>>,
    ) -> Self {
        Self {
            binance_client: Binance::new_with_config(pub_key, sec_key, config),
            current_order_id: 0,
            ws_buffer_size: 15,
            ws_tick: Some(WSTick::new(collect, live_price_watch)),
            account_info: None,
            exchange_info: vec![],
            live_ad,
            live_info,
            ..Default::default()
        }
    }
    async fn remove_api_keys(&mut self) -> Result<()> {
        self.binance_client = Account::new(None, None);
        Ok(())
    }
    async fn add_replace_api_keys(&mut self, pub_key: &str, priv_key: &str) -> Result<()> {
        let client: Account = Binance::new(Some(pub_key.to_string()), Some(priv_key.to_string()));
        let res = client.get_account().await;

        let live_inf = self.live_info.clone();
        let mut live_info = live_inf.lock().expect("live_info poisoned mutex");
        match res {
            Ok(_acc_info) => {
                tracing::trace!["API keys changed, account balances updated"];
                self.api_keys_valid = true;
                live_info.keys_status = KeysStatus::Valid;
                self.binance_client = client;
            }
            Err(e) => {
                tracing::error!["Unable to get binance account balances: {}", e];
                self.api_keys_valid = false;
                live_info.keys_status = KeysStatus::Invalid;
                return Err(anyhow!["{}", e]);
            }
        };
        Ok(())
    }
    async fn send_new_order(&mut self, sym: &str, o: &Order) -> Result<u64> {
        let order_request: OrderRequest =
            parse_to_binance(sym, &o, self.qoute_balances.0, self.base_balances.0);
        let transaction = self.binance_client.place_order(order_request).await?;
        let _res = self.get_all_balances().await;
        Ok(transaction.order_id)
    }
    //NOTE this is a horrible way to do this but for now it's fine
    pub async fn check_live_orders_change(live_info: Arc<Mutex<LiveInfo>>, binance: Account) {
        loop {
            let res = binance.get_all_open_orders().await;
            match res {
                Ok(orders) => {
                    let (
                        (a1_string, a2_string),
                        (a1_l_old, a2_l_old),
                        (a1_f_old, a2_f_old),
                        keys_status,
                    ) = {
                        let live_i = live_info.lock().expect("Live info mutex poisoned!");
                        (
                            live_i.current_pair_strings.clone(),
                            (live_i.current_pair_locked_balances),
                            (live_i.current_pair_free_balances),
                            live_i.keys_status,
                        )
                    };
                    match keys_status {
                        KeysStatus::Valid => {
                            let (a1_locked, a1_free) = if !&a1_string.is_empty() {
                                let res_a1 = binance.get_balance(&a1_string).await;
                                match res_a1 {
                                    Ok(balance) => (balance.locked, balance.free),
                                    Err(e) => {
                                        tracing::error![
                                            "check_live_orders_change ERROR: {} asset: {}",
                                            e,
                                            &a1_string
                                        ];
                                        (a1_l_old, a1_f_old)
                                    }
                                }
                            } else {
                                tracing::trace!["Unable to update balance for a1_string empty"];
                                (a1_l_old, a1_f_old)
                            };
                            let (a2_locked, a2_free) = if !&a2_string.is_empty() {
                                let res_a2 = binance.get_balance(&a2_string).await;
                                match res_a2 {
                                    Ok(balance) => (balance.locked, balance.free),
                                    Err(e) => {
                                        tracing::error![
                                            "check_live_orders_change ERROR: {} asset: {}",
                                            e,
                                            &a2_string
                                        ];
                                        (a2_l_old, a2_f_old)
                                    }
                                }
                            } else {
                                tracing::trace!["Unable to update balance for a1_string empty"];
                                (a2_l_old, a2_f_old)
                            };
                            tracing::trace!["a1_locked: {}, a2_locked: {}", a1_locked, a2_locked];
                            tracing::trace!["a1_free: {}, a2_free: {}", a1_free, a2_free];
                            let mut live_orders = HashMap::default();
                            let _res: Vec<_> = orders
                                .iter()
                                .map(|order_binance| {
                                    let res = from_binance_order(
                                        &order_binance,
                                        &a1_locked,
                                        &a2_locked,
                                        &a1_free,
                                        &a2_free,
                                    );
                                    match res {
                                        Ok(o) => {
                                            tracing::trace!["{:?}", &order_binance];
                                            live_orders
                                                .insert(order_binance.order_id, (o, true, 0.0));
                                        }
                                        Err(e) => {
                                            tracing::error!["check_live_orders_change {}", e];
                                        }
                                    };
                                })
                                .collect();
                            let mut live_inf = live_info.lock().expect("Live info mutex poisoned!");
                            live_inf.live_orders = live_orders;
                            live_inf.current_pair_locked_balances = (a1_locked, a2_locked);
                            live_inf.current_pair_free_balances = (a1_free, a2_free);
                            tracing::trace![
                                "live_inf (conn side) {:?}",
                                &live_inf.current_pair_free_balances
                            ];
                            tracing::trace![
                                "live_inf (conn side)locked {:?}",
                                &live_inf.current_pair_locked_balances
                            ];
                        }
                        KeysStatus::Invalid => {
                            sleep(Duration::from_millis(5_000)).await;
                        }
                    };
                }
                Err(e) => {
                    tracing::error!["check_live_orders_change {}", e];
                    sleep(Duration::from_millis(60_000)).await;
                }
            }
            sleep(Duration::from_millis(ORDER_CHECK_INTV_MS)).await;
        }
    }
    async fn cancel_order(&self, sym: &str, id: &u64) -> Result<()> {
        let order_cancelation = OrderCancellation {
            symbol: sym.to_string(),
            order_id: Some(*id),
            orig_client_order_id: None,
            new_client_order_id: None,
            recv_window: None,
        };
        let transaction = self.binance_client.cancel_order(order_cancelation).await?;
        tracing::trace!["{:?}", transaction];
        Ok(())
    }
    async fn cancel_all_orders(&self, symbol: &str) -> Result<()> {
        let transaction = self
            .binance_client
            .cancel_all_open_orders(symbol.to_string())
            .await?;
        tracing::trace!["Cancelled orders: {:?}", transaction];
        Ok(())
    }
    pub async fn connect_ws(
        mut ws_tick: WSTick,
        params: HashMap<String, Vec<String>>,
        buffer_size: usize,
    ) -> Result<()> {
        ws_tick.sub_params = params.clone();
        let _res = {
            let mut w = ws_tick
                .unsorted_output
                .lock()
                .expect("connect_ws unable to unlock uo mutex");
            w.3 = params;
        };
        let _res = tokio::task::spawn(async move {
            let _ = ws_tick.run(buffer_size).await;
        })
        .await?;
        Ok(())
    }
    pub async fn get_initial_data2(symbol: &str, live_ad: Arc<Mutex<AssetData>>) -> Result<()> {
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
        let ss = symbol.to_string();
        //FIXME try to get rid of the task spawning... this is kind of unnecessary
        let klines_unordered = Intv::iter()
            .map(move |i| {
                let s = ss.clone();
                let intv = i.clone();
                let client = reqwest::Client::new();
                tokio::task::spawn(async move { get_latest_wicks2(&client, &s, &intv).await })
            })
            .collect::<FuturesUnordered<_>>();
        let mut ku = pin![klines_unordered];

        let mut klines = Klines::new_empty();
        while let Some(res) = ku.next().await {
            match res {
                Ok((kline, intv)) => {
                    klines.insert(&intv, kline);
                    tracing::trace!["kline inserted for Symbol:{} Interval:{:?}", &symbol, intv];
                }
                Err(e) => {
                    tracing::error!["Kline JoinError:{}", e];
                }
            }
        }

        let mut live_b = live_ad
            .lock()
            .expect("Poisoned live AD mutex at get_initial_data");

        live_b.kline_data.insert(symbol.to_string(), klines);
        Ok(())
    }
    pub async fn refresh_balances(&mut self, symbol: &str, live_ad: Arc<Mutex<AssetData>>) {
        let res = self.get_open_orders_binance().await;
        match res {
            Ok(_) => (),
            Err(e) => {
                tracing::error!["Get binance orders error! {}", e];
            }
        };
        let res = self.get_balances(&symbol).await;
        match res {
            Ok(_) => (),
            Err(e) => {
                tracing::error!["Get balances error! {}", e];
            }
        };

        let res = get_asset_bases_binance(&symbol).await;
        match res {
            Ok(res2) => {
                match res2 {
                    Some((base, qoute)) => {
                        self.current_symbol_bases = (base, qoute);
                    }
                    None => (),
                };
            }
            Err(e) => {
                tracing::error!["get asset_bases ERROR: {}", e];
            }
        };
        let mut live_b = live_ad
            .lock()
            .expect("Poisoned live AD mutex at get_initial_data");
        live_b.acc_balances = self.balances.clone();
        live_b.live_asset_symbol_changed = (true, symbol.to_string());
        live_b.current_pair_strings = self.current_symbol_bases.clone();
        live_b.current_pair_free_balances = (self.base_balances.0, self.qoute_balances.0);
        live_b.current_pair_locked_balances = (self.base_balances.1, self.qoute_balances.1);
    }
    pub async fn get_initial_data(
        &mut self,
        symbol: &str,
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
        let ss = symbol.to_string();
        let klines_unordered = Intv::iter()
            .map(move |i| {
                let s = ss.clone();
                let intv = i.clone();
                let client = reqwest::Client::new();
                tokio::task::spawn(async move { get_latest_wicks2(&client, &s, &intv).await })
            })
            .collect::<FuturesUnordered<_>>();
        let mut ku = pin![klines_unordered];

        let mut klines = Klines::new_empty();
        while let Some(res) = ku.next().await {
            match res {
                Ok((kline, intv)) => {
                    klines.insert(&intv, kline);
                    tracing::trace!["kline inserted for Symbol:{} Interval:{:?}", &symbol, intv];
                }
                Err(e) => {
                    tracing::error!["Kline JoinError:{}", e];
                }
            }
        }
        let res = self.get_open_orders_binance().await;
        match res {
            Ok(_) => (),
            Err(e) => {
                tracing::error!["Get binance orders error! {}", e];
            }
        };
        let res = self.get_balances(&symbol).await;
        match res {
            Ok(_) => (),
            Err(e) => {
                tracing::error!["Get balances error! {}", e];
            }
        };

        let mut live_b = live_ad
            .lock()
            .expect("Poisoned live AD mutex at get_initial_data");

        live_b.kline_data.insert(symbol.to_string(), klines);
        live_b.acc_balances = self.balances.clone();
        live_b.current_pair_strings = self.current_symbol_bases.clone();
        live_b.current_pair_free_balances = (self.base_balances.0, self.qoute_balances.0);
        live_b.current_pair_locked_balances = (self.base_balances.1, self.qoute_balances.1);

        Ok(())
    }
    async fn get_open_orders_binance(&mut self) -> Result<()> {
        let orders = self.binance_client.get_all_open_orders().await?;
        let _res: Vec<_> = orders
            .iter()
            .map(|order_binance| {
                tracing::trace!["BINANCE ORDER{:?}", order_binance];
                let res = from_binance_order(
                    &order_binance,
                    &self.base_balances.1,
                    &self.qoute_balances.1,
                    &self.base_balances.0,
                    &self.qoute_balances.0,
                );
                match res {
                    Ok(o) => {
                        self.live_orders
                            .insert(order_binance.order_id, (o, true, 0.0));
                    }
                    Err(e) => {
                        tracing::error!["{}", e];
                    }
                };
            })
            .collect();
        Ok(())
    }
    async fn get_balances(&mut self, symbol: &str) -> Result<()> {
        let res = self.binance_client.get_account().await;
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
        let res = self.binance_client.get_account().await;
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
    pub async fn reconnect_ws(&mut self) -> Result<()> {
        tracing::trace!["reconnect_ws called"];
        let res = self
            .ws_tick
            .as_mut()
            .ok_or(anyhow!["WS tick should BE SOME disconnect WS"]);
        match res {
            Ok(ws_tick) => {
                let mut w = ws_tick
                    .unsorted_output
                    .lock()
                    .expect("Unable to unlock unsorted output");
                w.1 = false;
                w.2 = true;
            }
            Err(e) => {
                tracing::error!["reconnect_ws ERROR: {}", e];
            }
        }
        Ok(())
    }
    pub async fn disconnect_ws(&mut self) -> Result<()> {
        tracing::trace!["disconnect_ws called"];
        let res = self
            .ws_tick
            .as_mut()
            .ok_or(anyhow!["WS tick should BE SOME disconnect WS"]);
        match res {
            Ok(ws_tick) => {
                let mut w = ws_tick
                    .unsorted_output
                    .lock()
                    .expect("Unable to unlock unsorted output");
                tracing::trace!["ws.disconnect:{} ws.reconnect:{}", w.1, w.2];
                w.1 = true;
                w.2 = false;
            }
            Err(e) => {
                tracing::error!["disconnect_ws ERROR: {}", e];
            }
        }
        Ok(())
    }
    pub async fn get_user_data(&mut self) -> Result<()> {
        let resp = self.binance_client.get_account().await.context(ERR_CTX)?;
        tracing::trace!("{:?}", resp);
        self.account_info = Some(resp);
        Ok(())
    }
    pub async fn get_def_ws_params(
        &mut self,
        get_initial_data: bool,
    ) -> HashMap<String, Vec<String>> {
        let def_sym = self.default_symbol.clone();
        let p = self.get_ws_params(&def_sym, get_initial_data).await;
        p
    }
    pub async fn get_curr_ws_params(
        &mut self,
        get_initial_data: bool,
    ) -> HashMap<String, Vec<String>> {
        let def_sym = self.current_symbol.clone();
        let p = self.get_ws_params(&def_sym, get_initial_data).await;
        p
    }
    pub async fn get_ws_params(
        &mut self,
        symbol: &str,
        get_initial_data: bool,
    ) -> HashMap<String, Vec<String>> {
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

        if get_initial_data {
            let res = self.get_initial_data(&symbol, self.live_ad.clone()).await;
            match res {
                Ok(_) => {
                    tracing::trace!["Initial data for {} received", &symbol];
                    let mut ad = self
                        .live_ad
                        .lock()
                        .expect("get_ws_params: Unable to unlock mutex");
                    ad.live_asset_symbol_changed = (true, symbol.to_string());
                }
                Err(e) => tracing::error!["Initial data connection failed, ERROR: {}", e],
            };
        };
        params
    }
    pub fn update_settings(&mut self, settings: &Settings) -> Result<()> {
        self.default_symbol = settings.default_asset.clone();
        Ok(())
    }
    pub async fn parse_binance_instructs(&mut self, i: BinInstructs) -> BinResponse {
        match i {
            BinInstructs::SellAllNow { symbol } => {
                todo!()
            }
            BinInstructs::BuyAllNow { symbol } => {
                todo!()
            }
            BinInstructs::CancelOrder { id, symbol, .. } => {
                let res = self.cancel_order(&symbol, &id).await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!("{}", anyhow!["Unable to cancel_order:{}", e]);
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                let res = self.get_open_orders_binance().await;
                match res {
                    Ok(_) => (),
                    Err(e) => {
                        tracing::error!["Get binance orders error! {}", e];
                    }
                };
                let live_inf = self.live_info.clone();
                let live_i = live_inf.lock().expect("Unable to unlock live_info mutex");
                self.live_orders = live_i.live_orders.clone();
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
                        tracing::error!("{}", e);
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
                let ad_d = self.live_ad.clone();
                let symbol = self.current_symbol.clone();
                let _res = self.refresh_balances(&symbol, ad_d).await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!("{}", e);
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::ConnectWS { params: ref p } => {
                tracing::trace!["Connect ws start {:?}", &self];
                if let Some(ws_tick) = self.ws_tick.clone() {
                    let res =
                        BinanceClient::connect_ws(ws_tick.clone(), p.clone(), DEFAULT_BUFFER_SIZE)
                            .await;
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
                    tracing::trace!["Connect ws end {:?}", &self];
                    resp
                } else {
                    BinResponse::Failure(("Should be SOME here".to_string(), GeneralError::Generic))
                }
            }
            BinInstructs::ConnectUserWS { params: _ } => {
                tracing::trace!["Connect ws start {:?}", &self];
                BinResponse::Success
            }
            BinInstructs::ReConnectWS => {
                tracing::trace!["reconnect WS called!"];
                let _res = self.disconnect_ws().await;
                let _res = self.reconnect_ws().await;
                let symbol = self.current_symbol.clone();
                let ad_c = self.live_ad.clone();
                let _get_initial_data_handle = tokio::task::spawn(async move {
                    let _a = BinanceClient::get_initial_data2(&symbol, ad_c).await;
                });
                let ad_d = self.live_ad.clone();
                let symbol = self.current_symbol.clone();
                self.refresh_balances(&symbol, ad_d).await;
                tracing::trace!["reconnect WS finished!"];
                BinResponse::Success
            }
            BinInstructs::Disconnect => {
                tracing::trace!["Disconnect WS called!"];
                let _res = self.disconnect_ws().await;
                tracing::trace!["Disconnect WS finished!"];
                BinResponse::Success
            }
            BinInstructs::GetUserData => {
                tracing::trace!["Connect ws start {:?}", &self];
                let res = self.get_user_data().await;
                let resp = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!("{}", anyhow!["{:?} Unable to  e:{}", i, e]);
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::trace!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::PlaceOrder {
                symbol: ref s,
                o: order,
            } => {
                tracing::trace!["Connect ws start {:?}", &self];
                let res = self.send_new_order(&s, &order).await;
                let resp = match res {
                    Ok(_) => {
                        let res = self.get_open_orders_binance().await;
                        match res {
                            Ok(_) => (),
                            Err(e) => {
                                tracing::error!["Get binance orders error! {}", e];
                            }
                        };
                        let live_inf = self.live_info.clone();
                        let live_i = live_inf.lock().expect("Unable to unlock live_info mutex");
                        self.live_orders = live_i.live_orders.clone();
                        BinResponse::Success
                    }
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!("{}", anyhow!["Unable to place order ERROR: {}", e]);
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::trace!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::CancelAndReplaceOrder {
                id,
                symbol: ref s,
                o: order,
            } => {
                tracing::trace!["Connect ws start {:?}", &self];
                let res = self.cancel_order(&s, &id).await;
                match res {
                    Ok(_) => {}
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!("{}", anyhow!["Unable to cancel order:{}", e]);
                        return BinResponse::Failure((string_error, GeneralError::Generic));
                    }
                };
                let res = self.send_new_order(&s, &order).await;
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
                let res = self.get_open_orders_binance().await;
                match res {
                    Ok(_) => (),
                    Err(e) => {
                        tracing::error!["Get binance orders error! {}", e];
                    }
                };
                let live_inf = self.live_info.clone();
                let live_i = live_inf.lock().expect("Unable to unlock live_info mutex");
                self.live_orders = live_i.live_orders.clone();
                resp
            }
            BinInstructs::CancelAllOrders { symbol: ref s } => {
                let res = self.cancel_all_orders(&s).await;
                let resp = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        tracing::error!("Unable to cancel all orders! {}", e);
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                let res = self.get_open_orders_binance().await;
                match res {
                    Ok(_) => (),
                    Err(e) => {
                        tracing::error!["Get binance orders error! {}", e];
                    }
                };
                let live_inf = self.live_info.clone();
                let live_i = live_inf.lock().expect("Unable to unlock live_info mutex");
                self.live_orders = live_i.live_orders.clone();
                resp
            }
            BinInstructs::ChangeLiveAsset2 { symbol: ref s } => {
                let res = validate_asset_binance(s).await;
                self.current_symbol = s.clone();
                match res {
                    Ok(ans) => match ans {
                        true => (),
                        false => {
                            tracing::error!["Cannot find asset on binance:{}", s];
                            return BinResponse::Failure((
                                format!["Cannot find asset on binance:{}", s],
                                GeneralError::Generic,
                            ));
                        }
                    },
                    Err(e) => {
                        tracing::error!["Validate binance asset error:{}", e];
                        return BinResponse::Failure((format!["{}", e], GeneralError::Generic));
                    }
                };
                let res = self.ws_tick.as_mut();
                match res {
                    Some(ws_tick) => {
                        let mut w = ws_tick
                            .unsorted_output
                            .lock()
                            .expect("ChangeLiveAsset unable to unlock mutex uo");
                        w.4 = Some(s.to_string());
                    }
                    None => {
                        tracing::error!["WS tick should be SOME here, CHANGE LIVE ASSET"];
                    }
                };
                let ad_c = self.live_ad.clone();
                let ss = s.clone();
                let _get_initial_data_handle = tokio::task::spawn(async move {
                    let _a = BinanceClient::get_initial_data2(&ss, ad_c).await;
                });
                let ad_d = self.live_ad.clone();
                self.refresh_balances(s, ad_d).await;
                BinResponse::Success
            }
            BinInstructs::ChangeLiveAsset {
                symbol: ref s,
                defualt_symbol: _s,
            } => {
                let res = validate_asset_binance(s).await;
                match res {
                    Ok(ans) => match ans {
                        true => (),
                        false => {
                            tracing::error!["Cannot find asset on binance:{}", s];
                            return BinResponse::Failure((
                                format!["Cannot find asset on binance:{}", s],
                                GeneralError::Generic,
                            ));
                        }
                    },
                    Err(e) => {
                        tracing::error!["Validate binance asset error:{}", e];
                        return BinResponse::Failure((format!["{}", e], GeneralError::Generic));
                    }
                };
                let _res = self.disconnect_ws().await;
                let params = self.get_ws_params(s, false).await;
                tracing::trace!["Live re-connect params {:?}", params];
                let res = self.ws_tick.as_mut();
                match res {
                    Some(ws_tick) => {
                        ws_tick.sub_params = params.clone();
                        let mut w = ws_tick
                            .unsorted_output
                            .lock()
                            .expect("ChangeLiveAsset unable to unlock mutex uo");
                        w.3 = params;
                    }
                    None => {
                        tracing::error!["WS tick should be SOME here, CHANGE LIVE ASSET"];
                    }
                };
                self.current_symbol = s.clone();
                let ad_c = self.live_ad.clone();
                let ss = s.clone();
                let _get_initial_data_handle = tokio::task::spawn(async move {
                    let _a = BinanceClient::get_initial_data2(&ss, ad_c).await;
                });
                let ad_d = self.live_ad.clone();
                self.refresh_balances(&s, ad_d).await;
                let _res = self.reconnect_ws().await;
                tracing::trace!["reconnect WS finished!"];
                BinResponse::Success
            }
            BinInstructs::None => BinResponse::None,
        }
    }
}
//NOTE this is a really dumb way to reduce precision but it works and I haven't found better...
trait To6Fig<T> {
    fn to_6fig(&self) -> T;
}
impl To6Fig<f64> for f64 {
    fn to_6fig(&self) -> f64 {
        (self * 100_000.0).trunc() / 100_000.0
    }
}
impl To6Fig<f32> for f32 {
    fn to_6fig(&self) -> f32 {
        (self * 100_000.0).trunc() / 100_000.0
    }
}

fn parse_to_binance(sym: &str, o: &Order, a1: f64, a2: f64) -> OrderRequest {
    let (side, order_type, quantity, quote_order_qty, price, stop_price, time_in_force): (
        OrderSide,
        OrderType,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<TimeInForce>,
    ) = match o {
        Order::Market { buy: b, quant: q } => {
            let (side, quant) = match b {
                true => {
                    let quant = q.get_f64() * a1;
                    (OrderSide::Buy, quant.to_6fig())
                }
                false => {
                    let quant = q.get_f64() * a2;
                    return OrderRequest {
                        symbol: sym.to_string(),
                        side: OrderSide::Sell,
                        order_type: OrderType::Market,
                        time_in_force: None,
                        quantity: Some(quant),
                        quote_order_qty: None,
                        price: None,
                        new_client_order_id: None,
                        stop_price: None,
                        iceberg_qty: None,
                        new_order_resp_type: None,
                        recv_window: None,
                    };
                    //(OrderSide::Sell, quant.to_6fig())
                }
            };
            (side, OrderType::Market, None, Some(quant), None, None, None)
        }
        Order::Limit {
            buy: b,
            price: p,
            limit_status: _li,
            quant: q,
        } => {
            let (side, quant) = match b {
                true => {
                    let quant = q.get_f64() * a1 / p;
                    (OrderSide::Buy, quant.to_6fig())
                }
                false => {
                    let quant = q.get_f64() * a2;
                    tracing::debug!["limit_quant {}", quant];
                    (OrderSide::Sell, quant.to_6fig())
                }
            };
            (
                side,
                OrderType::Limit,
                Some(quant),
                None,
                Some(p.clone()),
                None,
                Some(TimeInForce::GTC),
            )
        }
        Order::StopLimit {
            buy: b,
            price: p,
            stop_price: sp,
            stop_status: _s,
            limit_status: _li,
            quant: q,
        } => {
            let (side, quant) = match b {
                true => {
                    let quant = q.get_f64() * a1 / p;
                    (OrderSide::Buy, quant.to_6fig())
                }
                false => {
                    let quant = q.get_f64() * a2;
                    (OrderSide::Sell, quant.to_6fig())
                }
            };
            (
                side,
                OrderType::StopLossLimit,
                Some(quant),
                None,
                Some(p.to_6fig().clone()),
                Some(sp.to_6fig().clone() as f64),
                Some(TimeInForce::GTC),
            )
        }
        Order::StopMarket {
            buy: b,
            price: p,
            stop_status: _s,
            quant: q,
        } => {
            let (side, quant) = match b {
                true => {
                    let quant = q.get_f64() * a1 / p;
                    (OrderSide::Buy, quant.to_6fig())
                }
                false => {
                    let quant = q.get_f64() * a2;
                    (OrderSide::Sell, quant.to_6fig())
                }
            };
            (
                side,
                OrderType::StopLoss,
                Some(quant),
                None,
                None,
                Some(p.to_6fig().clone() as f64),
                None,
            )
        }
        _ => panic!("Order type not yet implemented"),
    };
    OrderRequest {
        symbol: sym.to_string(),
        side,
        order_type,
        time_in_force,
        quantity,
        quote_order_qty,
        price: price,
        new_client_order_id: None,
        stop_price,
        iceberg_qty: None,
        new_order_resp_type: None,
        recv_window: None,
    }
}

fn from_binance_order(
    bo: &BinanceOrder,
    locked_a1: &f64,
    locked_a2: &f64,
    free_a1: &f64,
    free_a2: &f64,
) -> Result<Order> {
    let (side, quant) = match bo.side {
        OrderSide::Buy => {
            let quant = Quant::from_f64(
                bo.orig_qty * bo.price / (free_a2 + locked_a2), /*locked_a2/(free_a2+locked_a2)*/
            );
            (true, quant)
        }
        OrderSide::Sell => {
            let quant = Quant::from_f64(bo.orig_qty / (locked_a1 + free_a1));
            (false, quant)
        }
    };
    match bo.order_type {
        OrderType::Market => Ok(Order::Market { buy: side, quant }),
        OrderType::Limit => {
            let limit_status = {
                if bo.executed_qty == 0.0 {
                    LimitStatus::Untouched
                } else {
                    if 0.0 <= bo.executed_qty && bo.executed_qty <= bo.orig_qty {
                        LimitStatus::PartFilled {
                            percent_fill: (bo.orig_qty / bo.executed_qty) as f32,
                        }
                    } else {
                        LimitStatus::FullyFilled
                    }
                }
            };
            Ok(Order::Limit {
                buy: side,
                quant,
                price: bo.price,
                limit_status,
            })
        }
        OrderType::StopLoss => {
            let stop_status = {
                if bo.executed_qty == 0.0 {
                    StopStatus::Untouched
                } else {
                    StopStatus::Triggered
                }
            };
            let (side, quant) = match bo.side {
                OrderSide::Buy => {
                    let quant = Quant::from_f64(
                        bo.orig_qty * bo.stop_price / (free_a2 + locked_a2), /*locked_a2/(free_a2+locked_a2)*/
                    );
                    (true, quant)
                }
                OrderSide::Sell => {
                    let quant = Quant::from_f64(bo.orig_qty / (locked_a1 + free_a1));
                    (false, quant)
                }
            };
            Ok(Order::StopMarket {
                buy: side,
                quant,
                price: bo.stop_price,
                stop_status,
            })
        }
        OrderType::StopLossLimit => {
            let stop_status = {
                if bo.status != OrderStatus::Trade {
                    StopStatus::Untouched
                } else {
                    StopStatus::Triggered
                }
            };
            let limit_status = {
                if bo.executed_qty == 0.0 {
                    LimitStatus::Untouched
                } else {
                    if 0.0 <= bo.executed_qty && bo.executed_qty <= bo.orig_qty {
                        LimitStatus::PartFilled {
                            percent_fill: (bo.orig_qty / bo.executed_qty) as f32,
                        }
                    } else {
                        LimitStatus::FullyFilled
                    }
                }
            };
            Ok(Order::StopLimit {
                buy: side,
                quant,
                price: bo.price,
                limit_status,
                stop_price: bo.stop_price as f32,
                stop_status,
            })
        }
        _ => Err(anyhow!["Unsupporder order type from Binance!"]),
    }
}
fn get_ws_params2(symbol: &str) -> Vec<String> {
    let mut sub_params = vec![format!["{}@aggTrade", symbol.to_lowercase()]];
    for i in Intv::iter() {
        sub_params.push(format![
            "{}@kline_{}",
            symbol.to_lowercase(),
            i.to_bin_str()
        ])
    }
    sub_params
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    //TODO  make binance api tests
    async fn example() {}
}
