use binance_async::Binance;
use binance_async::models::*;
use binance_async::rest::spot::GetAccountRequest;
use binance_async::rest::usdm::{self, StartUserDataStreamRequest, StartUserDataStreamResponse};

use binance_async::websocket::{
    AccountUpdate, AccountUpdateBalance, AggregateTrade, BinanceWebsocket, CandelStickMessage,
    Depth, MiniTicker, Ticker, TradeMessage, UserOrderUpdate, usdm::WebsocketMessage,
};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

use std::time::{SystemTime, UNIX_EPOCH};

use crate::{BinInstructs, BinResponse, GeneralError};

use crate::data::AssetData;
use crate::data::Klines;
use crate::data::get_data_binance;

use tokio::sync::mpsc::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use anyhow::{Context, Result, anyhow};
use std::result::Result as OCResult;

use crate::data::Intv;

use futures::StreamExt;

use rust_decimal::Decimal;

use derive_debug::Dbg;
use num::FromPrimitive;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::watch::{Receiver, Sender};
use tracing::instrument;
use websockets::WebSocket;

use binance::api::Binance as Binance2;
use binance::market::Market;
use binance::model::{KlineSummaries, KlineSummary};

use crate::trade::Order;

const err_ctx: &str = "Binance client | websocket:";

/*
      "lastUpdateId": 1027024,
      "E": 1589436922972,   // Message output time
      "T": 1589436922959,   // Transaction time
      "bids": [
        [
          "4.00000000",     // PRICE
          "431.00000000"    // QTY
        ]
      ],
      "asks": [
        [
          "4.00000200",
          "12.00000000"
        ]
      ]
    }
{
  "id": "9d32157c-a556-4d27-9866-66760a174b57",
  "status": 200,
  "result": {
    "lastUpdateId": 1027024,
    "symbol": "BTCUSDT",
    "bidPrice": "4.00000000",
    "bidQty": "431.00000000",
    "askPrice": "4.00000200",
    "askQty": "9.00000000",
    "time": 1589437530011   // Transaction time
  },
  "rateLimits": [
    {
      "rateLimitType": "REQUEST_WEIGHT",
      "interval": "MINUTE",
      "intervalNum": 1,
      "limit": 2400,
      "count": 2
    }
  ]
}
* */
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct OrderBookWS {
    bid_price: f64,
    bid_qnt: f64,
    ask_price: f64,
    ask_qnt: f64,
    transaction_time: u64,
}
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
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
pub struct KlineTick{
    t: i64, // open time 
    T: i64, // close time
    //s: String, //symbol
    //i: String, //Interval
    f: i64, //??
    L: i64,// ??
    o: f64, 
    c: f64,
    h: f64,
    l: f64,
    v: f64,
    n: i64, //no of trades
    x: bool, //kline open or close
    q: f64, //qout taer volume
    V: f64,
    Q: f64,
    B: i64, //ignore
}
impl KlineTick{
    pub fn to_kline_vec(input:&[KlineTick])->Vec<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>{
        input.iter().map(|i|{
            let open_time=chrono::NaiveDateTime::from_timestamp_millis(i.t).expect("Fukcckckck");
            (open_time, i.o, i.h, i.l, i.c, i.v)
        }).collect()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct TradeWS {}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BinWSResponse {
    OrderBook(OrderBookWS),
    AggTrade(AggTradeWS),
    Trade(TradeWS),
    KlineTick((Intv,KlineTick)),
}
impl Default for BinWSResponse {
    fn default() -> BinWSResponse {
        BinWSResponse::AggTrade(AggTradeWS::default())
    }
}

impl BinWSResponse {
    #[instrument(level = "debug")]
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
            BinWSResponse::KlineTick((intv,_)) => {
                format!["{}@kline_{}", symbol.to_lowercase(), intv.to_bin_str()]
            }
        };
        tracing::debug!["{}", &ret];
        ret
    }
    #[instrument(level = "trace")]
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
                let t_now=SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
                #[cfg(debug_assertions)]
                let event_time=input["E"].as_i64().ok_or(anyhow!["Unable to parse E as i64"])?;
                tracing::trace!["\x1b[34m (AGGTrade Current linux epoch - event_time)\x1b[0m: {}ms", t_now-event_time];
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
                tracing::trace!["Parsing kline: {}",&input["k"]];
                let symbol = input
                    .get("s")
                    .ok_or(anyhow!["s Symbol not found in input :{:?}", input])?
                    .as_str()
                    .ok_or(anyhow!["Unable to parse s"])?
                    .to_string();
                tracing::trace!["Parsing kline 2: {}",&input["k"]];
                let interval= input["k"].
                    get("i")
                    .ok_or(anyhow!["i Interval not found in input :{:?}", input])?
                    .as_str()
                    .ok_or(anyhow!["Unable to parse i"])?;
                    
                let i=Intv::from_bin_str(interval);
                let input2=&input["k"];

                #[cfg(debug_assertions)]
                let t_now=SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
                #[cfg(debug_assertions)]
                let event_time=input["E"].as_i64().ok_or(anyhow!["Unable to parse E as i64"])?;
                tracing::trace!["\x1b[31m (KLINE Current linux epoch - event_time)\x1b[0m: {}ms", t_now-event_time];
                /*
                */
                Ok((
                    symbol,
                    BinWSResponse::KlineTick((i,
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
                    ))
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
    pub closed_klines: HashMap<Intv,Vec<KlineTick>>,
    pub all_klines: HashMap<Intv,Vec<KlineTick>>,
}

#[derive(Debug, Default)]
struct WSTick {
    unsorted_output: Mutex<Vec<(String, BinWSResponse)>>,
    send_price: Sender<f64>,
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
    #[instrument(level = "trace")]
    async fn sort(&mut self) -> Vec<(String, BinWSResponse)> {
        let mut uo = self.unsorted_output.lock().expect("Unable to unlock mutex: binance::sort()");
        uo.drain(..).collect::<Vec<(String, BinWSResponse)>>()
    }
    #[instrument(level = "trace")]
    async fn place_unsorted(&mut self, symbol: String, input: BinWSResponse) -> Result<()> {
        tracing::trace!["Binws response: {:?}",input];
        match input {
            BinWSResponse::OrderBook(o) => {
                let mut cum_queue = self
                    .symbol_sorted_output
                    .lock()
                    .expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(mut queue) = cum_queue.get_mut(&symbol) {
                    queue.order_book.push(o.clone());
                } else {
                    let mut queue = SymbolOutput::default();
                    queue.order_book.push(o.clone());
                    cum_queue.insert(symbol, queue);
                };
            }
            BinWSResponse::AggTrade(o) => {
                let mut cum_queue = self
                    .symbol_sorted_output
                    .lock()
                    .expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(mut queue) = cum_queue.get_mut(&symbol) {
                    queue.agg_trade.push(o.clone());
                    tracing::trace!["Agg trade queue {:?}", &queue];
                } else {
                    let mut queue = SymbolOutput::default();
                    queue.agg_trade.push(o.clone());
                    cum_queue.insert(symbol, queue);
                };
            }
            BinWSResponse::Trade(o) => {
                let mut cum_queue = self
                    .symbol_sorted_output
                    .lock()
                    .expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(mut queue) = cum_queue.get_mut(&symbol) {
                    queue.trade.push(o.clone());
                } else {
                    let mut queue = SymbolOutput::default();
                    queue.trade.push(o.clone());
                    cum_queue.insert(symbol, queue);
                };
            }
            BinWSResponse::KlineTick((intv, kline)) => {
                let mut cum_queue = self
                    .symbol_sorted_output
                    .lock()
                    .expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(mut queue) = cum_queue.get_mut(&symbol) {
                    if kline.x==true{
                        tracing::debug!["\x1b  CLOSED kline \x1b[93m  = {:?}", &kline];
                        if let Some(mut kline_closed_queue)= queue.closed_klines.get_mut(&intv){
                            kline_closed_queue.push(kline.clone());
                            //clear the tick queue once the kline is closed
                            tracing::debug!["\x1b kline_tick CLOSED queue size \x1b[93m  = {:?}", kline_closed_queue];
                            queue.all_klines.insert(intv.clone(),vec![]);
                            if let Some(mut kline_all_queue)= queue.all_klines.get_mut(&intv){
                                kline_all_queue.clear();
                                tracing::trace!["\x1b kline_tick OPEN clear() ran! \x1b[93m  "];

                            };
                            //Notify that a kline is closed - close time
                        }else{
                            queue.closed_klines.insert(intv.clone(),vec![kline.clone()]);
                            tracing::trace!["\x1b kline_tick CLOSED queue size :1 - initiated\x1b[93m"];
                            //plot closed_klines directly and fill any gaps
                        };
                    }else{
                        if let Some(mut kline_all_queue)= queue.all_klines.get_mut(&intv){
                            kline_all_queue.push(kline.clone());
                            tracing::trace!["\x1b kline_tick OPEN queue size \x1b[93m  = {:?}", &kline_all_queue.len()];
                        }else{
                            queue.all_klines.insert(intv.clone(),vec![kline.clone()]);
                            tracing::trace!["\x1b kline_tick OPEN queue size :1 - initiated\x1b[93m"];
                        };

                    };
                } else {
                    let mut queue = SymbolOutput::default();
                    if kline.x==true{
                        queue.closed_klines.insert(intv.clone(),vec![kline.clone()]);
                    }else{
                        queue.all_klines.insert(intv.clone(),vec![kline.clone()]);
                    };
                    cum_queue.insert(symbol, queue);
                };
            }
        }
        Ok(())
    }
    #[instrument(level = "trace")]
    async fn append_tick(&mut self, input: Value) -> Result<()> {
        tracing::trace!["\x1b[93m Raw ws output\x1b[93m : {:?}", input];
        let (symbol, sorted) = BinWSResponse::parse_into_by_str(input).context("Failed to parse into str")?;
        match (sorted, self.watch_price_ty) {
            (BinWSResponse::Trade(val), BinWSResponse::Trade(_)) => {
                //todo!()
            }
            (BinWSResponse::AggTrade(val), BinWSResponse::AggTrade(_)) => {
                let mut lp1=self.live_price1.lock().expect("Unable to unlock mutex: binance::append_tick()");
                *lp1=val.p.clone();
            }
            (BinWSResponse::OrderBook(val), BinWSResponse::AggTrade(_)) => {
                //todo!()

            }
            (BinWSResponse::KlineTick((_,val)), BinWSResponse::KlineTick((_,_))) => {
                //todo!()
            }
            _ => (),
        }
        let mut uo = self.unsorted_output.lock().expect("Unable to unlock mutex: binance::append_tick()");
        tracing::trace!["Placing ws output"];
        uo.push((symbol, sorted));
        tracing::trace!["Raw ws successfully placed"];
        Ok(())
    }
    fn get_data_ref(&self) -> Arc<Mutex<HashMap<String, SymbolOutput>>> {
        Arc::clone(&self.symbol_sorted_output)
    }
    #[instrument(level = "trace")]
    async fn connect(&self) -> Result<(WebSocket)> {
        let mut p = self.sub_params.clone();
        tracing::debug!["\x1b[93m Subscribe message hashmap\x1b[93m : {:?}", p];
        let mut ovec: Vec<String> = vec![];
        p.iter_mut()
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
        log::info!["Web socket connected with message:{:?}", sub_message];
        Ok(conn)
    }
    #[instrument(level = "trace")]
    async fn collect_output(&mut self, buffer_size: usize, conn: &mut WebSocket) -> Result<()> {
        let mut n = 0;
        loop {
            while n < buffer_size {
                let msg = conn.receive().await?;
                tracing::trace!["\x1b[93m WS raw output: \x1b[0m {:?}", msg];
                if let Some((m, _, _)) = msg.clone().into_text() {
                    let v: Value = serde_json::from_str(&m)?;
                    if let Some(id) = v.get("id") {
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
                self.place_unsorted(symbol, response).await.context("Unable to sort WS output")?;
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
    #[instrument(level = "trace")]
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
    binance_client: Binance,
    pub_key: String,
    sec_key: String,
    current_order_id: u64,
    ws_buffer_size: usize,
    ws_tick: WSTick,
    account_info: Option<AccountInformation>,
}

impl Default for BinanceClient {
    fn default() -> Self {
        Self {
            binance_client: Binance::default(),
            pub_key: "".to_string(),
            sec_key: "".to_string(),
            current_order_id: 0,
            ws_buffer_size: 50,
            ws_tick: WSTick::default(),
            account_info: None,
        }
    }
}

impl BinanceClient {
    pub fn new(
        pub_key: String,
        sec_key: String,
        collect: Arc<Mutex<HashMap<String, SymbolOutput>>>,
        live_price_watch: Arc<Mutex<f64>>,
    ) -> Self {
        Self {
            binance_client: Binance::with_key_and_secret(&pub_key, &sec_key),
            pub_key: pub_key,
            sec_key: sec_key,
            current_order_id: 0,
            ws_buffer_size: 15,
            ws_tick: WSTick::new(collect, live_price_watch),
            account_info: None,
        }
    }
    #[instrument(level = "debug")]
    async fn send_new_order(
        &self,
        sym: &str,
        otype: OrderType,
        sid: OrderSide,
        pric: f64,
        quant: f64,
    ) -> Result<()> {
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
            .context(err_ctx)?;
        log::info!("{:?}", resp);
        Ok(())
    }
    #[instrument(level = "debug")]
    async fn cancel_order(&self, sym: &str) -> Result<()> {
        let resp = self
            .binance_client
            .request(usdm::CancelOrderRequest {
                symbol: sym.into(),
                order_id: Some(self.current_order_id),
                ..Default::default()
            })
            .await
            .context(err_ctx)?;
        log::info!("{:?}", resp);
        Ok(())
    }
    #[instrument(level = "debug")]
    async fn start_user_stream(&self) -> Result<()> {
        let listen_key = self
            .binance_client
            .request(StartUserDataStreamRequest {})
            .await
            .context(err_ctx)
            .context("Binance client:unable to connect_ws user stream")?;
        let mut ws: BinanceWebsocket<WebsocketMessage> =
            BinanceWebsocket::new(&[listen_key.listen_key.as_str()])
                .await
                .context(err_ctx)?;
        loop {
            let msg = ws
                .next()
                .await
                .ok_or(anyhow!["Ws exited on a None"])
                .context(err_ctx)?;
            log::debug!("{msg:?}");
        }
        Ok(())
    }
    #[instrument(level = "trace")]
    pub async fn connect_ws(&mut self, params: HashMap<String, Vec<String>>) -> Result<()> {
        //NOTE may have to get rid this self due to inter mutability,,,
        self.ws_tick.sub_params = params;
        self.ws_tick
            .run(self.ws_buffer_size)
            .await
            .context(err_ctx)?;
        tracing::trace!["Connect WS ws: {:?}", &self];
        Ok(())
    }
    pub async fn get_initial_data(
        &mut self,
        symbol: &str,
        default_intv: &Intv,
        limit: usize,
        live_ad: Arc<Mutex<AssetData>>,
    ) -> Result<()> {
        let client: Market = Binance2::new(None, None);
        let mut klines = Klines::new_empty();
        let kl_0 = get_data_binance(&client, &symbol, default_intv.clone(), None)?;
        klines.insert_fat(&default_intv, kl_0);
        let mut live_a = live_ad
            .lock()
            .expect("Poisoned live AD mutex at get_initial_data");
        live_a.kline_data.insert(symbol.to_string(), klines);
        let mut klines = Klines::new_empty();
        for i in Intv::iter() {
            if i != *default_intv {
                let kl = get_data_binance(&client, &symbol, i, None)?;
                klines.insert_fat(&i, kl);
            };
        }
        let mut live_b = live_ad
            .lock()
            .expect("Poisoned live AD mutex at get_initial_data");
        live_b.kline_data.insert(symbol.to_string(), klines);
        Ok(())
    }
    #[instrument(level = "debug")]
    async fn change_ws_params(&mut self, new_params: HashMap<String, Vec<String>>) -> Result<()> {
        //TODO check if possible to change params without disconnecting
        self.ws_tick.disconnect = true;
        self.ws_tick.sub_params = new_params;
        self.ws_tick
            .run(self.ws_buffer_size)
            .await
            .context(err_ctx)?;
        tracing::debug!["{:?}", &self];
        Ok(())
    }
    #[instrument(level = "debug")]
    pub async fn disconnect_ws(&mut self) {
        tracing::debug!["{:?}", &self];
        self.ws_tick.disconnect = true;
        tracing::debug!["{:?}", &self];
    }
    #[instrument(level = "debug")]
    pub async fn get_user_data(&mut self) -> Result<()> {
        let resp = self
            .binance_client
            .request(GetAccountRequest {})
            .await
            .context(err_ctx)?;
        tracing::debug!["{:?}", &self];
        log::info!("{:?}", resp);
        self.account_info = Some(resp);
        Ok(())
    }
    #[instrument(level = "debug")]
    pub async fn parse_binance_instructs(&mut self, i: BinInstructs) -> BinResponse {
        //TODO query state here...
        match i {
            BinInstructs::ConnectWS { params: ref p } => {
                tracing::debug!["Connect ws start {:?}", &self];
                let res = self.connect_ws(p.clone()).await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        log::error!(
                            "{}",
                            anyhow![
                                "{:?} Unable to connect to ws  e:{}",
                                i.clone(),
                                e.context(err_ctx)
                            ]
                        );
                        //TODO check internet connection here with max retryies
                        //BinResponse::Failure((string_error,GeneralError::SystemError(Sys_err::No_Network)))
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::ConnectUserWS { params: ref p } => {
                tracing::debug!["Connect ws start {:?}", &self];
                let res = self.start_user_stream().await;
                let resp: BinResponse = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(err_ctx)]
                        );
                        //TODO check internet connection here with max retryies
                        //BinResponse::Failure((string_error,GeneralError::SystemError(Sys_err::No_Network)))
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::Disconnect => {
                let res = self.disconnect_ws().await;
                BinResponse::Success
            }
            BinInstructs::GetUserData => {
                tracing::debug!["Connect ws start {:?}", &self];
                let res = self.get_user_data().await;
                let resp = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        log::error!("{}", anyhow!["{:?} Unable to  e:{}", i, e]);
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
                let (order_type, order_side, price, stop_price, quantity) =
                    parse_order_to_ba(&order);
                let res = self
                    .send_new_order(&s, order_type, order_side, price, quantity)
                    .await;
                let resp = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(err_ctx)]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::CancelAndReplaceOrder {
                symbol: ref s,
                o: order,
            } => {
                tracing::debug!["Connect ws start {:?}", &self];
                let res = self.cancel_order(&s).await;
                match res {
                    Ok(_) => {}
                    Err(e) => {
                        let string_error = format!["{}", e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(err_ctx)]
                        );
                        return BinResponse::Failure((string_error, GeneralError::Generic));
                    }
                };
                let (order_type, order_side, price, stop_price, quantity) =
                    parse_order_to_ba(&order);
                let res = self
                    .send_new_order(&s, order_type, order_side, price, quantity)
                    .await;
                let resp = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(err_ctx)]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}", &self];
                resp
            }
            BinInstructs::CancelAllOrders { symbol: ref s } => {
                //TODO rewrite this to support multiple orders
                let res = self.cancel_order(&s).await;
                let resp = match res {
                    Ok(_) => BinResponse::Success,
                    Err(e) => {
                        let string_error = format!["{}", e];
                        log::error!(
                            "{}",
                            anyhow![
                                "{:?} Unable to sell_now e:{}",
                                i.clone(),
                                e.context(err_ctx)
                            ]
                        );
                        BinResponse::Failure((string_error, GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::None => BinResponse::None,
        }
    }
}
//TODO AnyhowContext one level above
#[instrument(level = "debug")]
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
#[derive(Deserialize, Debug)]
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
    use serde_json::json;
    use websockets::WebSocket;

    #[tokio::test]
    //TODO split into different streams types and write parsing functions
    async fn init_ws() -> Result<()> {
        let params = ["btcustd@aggTrade", "btcusdt@bookTicker"];
        let socket = "wss://stream.binance.com:9443/ws";
        let sub_message = json!({
            "method": "SUBSCRIBE",
            "id": 1,
            "params":["btcusdt@aggTrade"],
        });
        let mut conn = WebSocket::connect(socket).await.unwrap();
        let msg = &conn.send_text(sub_message.to_string()).await;
        match msg {
            Ok(()) => {}
            Err(e) => panic!("{}", e),
        }
        for _ in 0..10 {
            let msg = &conn.receive().await;
            match msg {
                Ok(msg) => {
                    let (m, _, _) = msg.clone().into_text().unwrap();
                    let v: Value = serde_json::from_str(&m).unwrap();
                    let s = v["p"].as_str();
                    let k: &str;
                    let e = v["e"].as_str();
                    match e {
                        Some(kk) => k = kk,
                        None => continue,
                    }
                    match k {
                        "aggTrade" => todo!(),
                        "bookTicker" => todo!(),
                        _ => todo!(),
                    }
                    let p: f64;
                    match s {
                        Some(price) => {
                            let pp = price.parse::<f64>();
                            match pp {
                                Ok(ppp) => p = ppp,
                                Err(_) => continue,
                            }
                        }
                        None => continue,
                    }
                    println!("{:?}", p);
                }
                Err(e) => {
                    println!("{}", e);
                }
            };
        }
        assert!(true);
    }
}
