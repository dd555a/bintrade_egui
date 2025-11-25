use binance_async::Binance;
use binance_async::models::*;
use binance_async::rest::spot::GetAccountRequest;
use binance_async::rest::usdm::{
    self, StartUserDataStreamRequest, StartUserDataStreamResponse,
};
use binance_async::websocket::{
    AccountUpdate, AccountUpdateBalance, AggregateTrade, BinanceWebsocket,
    CandelStickMessage, Depth, MiniTicker, Ticker, TradeMessage,
    UserOrderUpdate, usdm::WebsocketMessage,
};

use crate::{BinInstructs,BinResponse, GeneralError};


use tokio::sync::mpsc::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use anyhow::{Context, Result, anyhow};
use std::result::Result as OCResult;

use futures::StreamExt;

use rust_decimal::Decimal;

use num::FromPrimitive;
use std::sync::{Arc,Mutex as SMutex};
use tokio::sync::Mutex;
use tokio::sync::watch::{Receiver, Sender};
use serde_json::json;
use std::collections::HashMap;
use websockets::WebSocket;
use tracing::instrument;
use derive_debug::Dbg;

use crate::trade::Order;

const err_ctx:&str="Binance client | websocket:";


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
    bid_price:f64,
    bid_qnt:f64,
    ask_price:f64,
    ask_qnt:f64,
    transaction_time:u64,
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
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct TradeWS {}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BinWSResponse {
    OrderBook(OrderBookWS),
    AggTrade(AggTradeWS),
    Trade(TradeWS),
}
impl Default for BinWSResponse {
    fn default() -> BinWSResponse {
        BinWSResponse::Trade(TradeWS::default())
    }
}

impl BinWSResponse {
    #[instrument(level="debug")]
    fn subscribe_key(&self, symbol: &str) -> String {
        let ret=match &self {
            BinWSResponse::OrderBook(_) => {
                format!["{}@bookTicker", symbol.to_lowercase()]
            }
            BinWSResponse::AggTrade(_) => {
                format!["{}@aggTrade", symbol.to_lowercase()]
            }
            BinWSResponse::Trade(_) => {
                format!["{}@Trade", symbol.to_lowercase()]
            }
        };
        tracing::debug!["{}",&ret];
        ret
    }
    #[instrument(level="debug")]
    fn parse_into_by_str(input: Value) -> Result<(String,BinWSResponse)> {
        let ws_output_type = input["e"]
            .as_str()
            .ok_or(anyhow!["E not found in binance output: {:}?",input])?;
        tracing::debug!["\x1b[93m Input Value \x1b[0m: {:?}",&input];
        let ret=match ws_output_type {
            "bookTicker" => {
                //BinWSResponse::OrderBook(_);
                todo!()
            }
            "aggTrade" => {
                let symbol=input.get("s").ok_or(anyhow!["s Symbol not found in input :{:?}",input])?.as_str().ok_or(anyhow!["Unable to parse s"])?.to_string();
                Ok((symbol,BinWSResponse::AggTrade(
                        //NOTE this code makes me want to get aids...
                        AggTradeWS{
                            E:input["E"].as_i64().ok_or(anyhow!["Unable to parse E"])?,
                            M:input["M"].as_bool().ok_or(anyhow!["Unable to parse M"])?,
                            T:input["T"].as_i64().ok_or(anyhow!["Unable to parse T"])?,
                            a:input["a"].as_i64().ok_or(anyhow!["Unable to parse a"])?,
                            f:input["f"].as_i64().ok_or(anyhow!["Unable to parse f"])?,
                            l:input["l"].as_i64().ok_or(anyhow!["Unable to parse l"])?,
                            p:input["p"].as_str().ok_or(anyhow!["Unable to parse p"])?.parse()?,
                            q:input["q"].as_str().ok_or(anyhow!["Unable to parse q"])?.parse()?,
                            m:input["m"].as_bool().ok_or(anyhow!["Unable to parse "])?,
                        })
                ))
            }
            _ => Err(anyhow!["Unable to parse ws output:{:?}", input]),
        };
        tracing::debug!["{:?}",&ret];
        ret
    }
}


#[derive(Debug, Default, Clone)]
pub struct SymbolOutput {
    trade: Vec<TradeWS>,
    agg_trade: Vec<AggTradeWS>,
    order_book: Vec<OrderBookWS>,
}

#[derive(Debug, Default)]
struct WSTick {
    unsorted_output: Mutex<Vec<(String,BinWSResponse)>>,
    send_price: Sender<f64>,
    watch_price_ty: BinWSResponse,
    sub_params: HashMap<String, Vec<String>>,
    symbol_sorted_output: Arc<SMutex<HashMap<String, SymbolOutput>>>,
    live_price1:Arc<Mutex<f64>>,
    disconnect: bool,
}
impl WSTick {
    fn new(symbol_sorted_output: Arc<SMutex<HashMap<String, SymbolOutput>>>, live_price1:Arc<Mutex<f64>>, send_price:Sender<f64>)->Self{
        Self{
            symbol_sorted_output,
            live_price1,
            ..Default::default()
        }
    }
    #[instrument(level="trace")]
    async fn sort(&mut self) -> Vec<(String,BinWSResponse)> {
        let mut uo = self.unsorted_output.lock().await;
        uo.drain(..)
            .collect::<Vec<(String,BinWSResponse)>>()
    }
    #[instrument(level="trace")]
    async fn place_unsorted(
        &mut self,
        symbol:String,
        input: BinWSResponse,
    ) -> Result<()> {
        match input {
            BinWSResponse::OrderBook(o) => {
                let mut cum_queue = self.symbol_sorted_output.lock().expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(mut queue) = cum_queue.get_mut(&symbol){
                    queue.order_book.push(o.clone());
                }else{
                    let mut queue=SymbolOutput::default();
                    queue.order_book.push(o.clone());
                    cum_queue.insert(symbol,queue);
                };
            }
            BinWSResponse::AggTrade(o) => {
                let mut cum_queue = self.symbol_sorted_output.lock().expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(mut queue) = cum_queue.get_mut(&symbol){
                    queue.agg_trade.push(o.clone());
                    tracing::trace!["Agg trade queue {:?}",&queue];
                }else{
                    let mut queue=SymbolOutput::default();
                    queue.agg_trade.push(o.clone());
                    cum_queue.insert(symbol,queue);
                };
            }
            BinWSResponse::Trade(o) => {
                let mut cum_queue = self.symbol_sorted_output.lock().expect("(BINCLIENT) poisoned data collection mutex");
                if let Some(mut queue) = cum_queue.get_mut(&symbol){
                    queue.trade.push(o.clone());
                }else{
                    let mut queue=SymbolOutput::default();
                    queue.trade.push(o.clone());
                    cum_queue.insert(symbol,queue);
                };
            }
        }
        Ok(())
    }
    async fn append(&mut self, input: Value) -> Result<()> {
        let (symbol,sorted) = BinWSResponse::parse_into_by_str(input)?;
        match (sorted, self.watch_price_ty) {
            (BinWSResponse::Trade(val), BinWSResponse::Trade(_)) =>{
                todo!()
            },
            (BinWSResponse::AggTrade(val), BinWSResponse::AggTrade(_)) => {
                self.send_price.send(val.p.clone());
            }
            (BinWSResponse::OrderBook(val), BinWSResponse::AggTrade(_)) => {
                todo!()
            }
            _ => (),
        }
        let mut uo = self.unsorted_output.lock().await;
        uo.push((symbol,sorted));
        Ok(())
    }
    fn get_data_ref(&self) -> Arc<SMutex<HashMap<String, SymbolOutput>>> {
        Arc::clone(&self.symbol_sorted_output)
    }
    #[instrument(level="debug")]
    async fn connect(&self) -> Result<(WebSocket)> {
        let mut p = self.sub_params.clone();
        tracing::debug!["\x1b[93m Subscribe message hashmap\x1b[93m : {:?}",p];
        let mut ovec: Vec<String> = vec![];
        p.iter_mut().map(|(_, mut out)| {
            tracing::debug!["\x1b[93m Iter out\x1b[93m : {:?}",out];
            ovec.append(&mut out);
        }).collect::<()>();
        //let params = ["btcustd@Trade","btcustd@aggTrade", "btcusdt@bookTicker"];
        tracing::debug!["\x1b[93m Ovec \x1b[0m: {:?}",ovec];
        let socket = "wss://stream.binance.com:9443/ws";
        let sub_message = json!({
            "method": "SUBSCRIBE",
            "id": 1,
            "params":ovec.as_slice(), //NOTE press x to doubt...
        });
        tracing::debug!["\x1b[93m Subscribe message\x1b[0m :  {:?}",sub_message];
        let mut conn = WebSocket::connect(socket).await?;
        conn.send_text(sub_message.to_string()).await?;
        log::info!["Web socket connected with message:{:?}", sub_message];
        Ok(conn)
    }
    #[instrument(level="debug")]
    async fn collect_output(
        &mut self,
        buffer_size: usize,
        conn: &mut WebSocket,
    ) -> Result<()> {
        let mut n = 0;
        loop {
            while n < buffer_size {
                let msg = conn.receive().await?;
                tracing::debug!["\x1b[93m WS output: \x1b[0m {:?}" ,msg];
                if let Some((m, _, _)) = msg.clone().into_text(){
                    let v: Value = serde_json::from_str(&m)?;
                    if let Some(id)=v.get("id"){
                        tracing::debug!["\x1b[93m WS output (NOT APPEND): \x1b[0m {:?}",msg];
                    }else{
                        self.append(v).await?;
                        tracing::debug!["\x1b[93m WS output (APPEND): \x1b[0m {:?}",msg];
                        n += 1;
                    }
                }
            }
            let mut buffer = self.sort().await;
            while let Some((symbol,response)) = buffer.pop() {
                self.place_unsorted(symbol,response).await?;
            };
            n=0;
            tracing::debug!["\x1b[93m Unsorted  messages placed \x1b[0m" ];
            //TODO start multiple websockets for multiple symbols?
            if self.disconnect == true {
                tracing::debug!["\x1b[93m WS disconnect called!\x1b[0m" ];
                break;
            }
        }
        conn.close(None).await?;
        self.disconnect = false;
        Ok(())
    }
    #[instrument(level="debug")]
    async fn run(&mut self, buffer_size: usize) -> Result<()> {
        let mut conn = self.connect().await?;
        self.collect_output(buffer_size, &mut conn).await?;
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

impl Default for BinanceClient{
    fn default()->Self{
        Self{
            binance_client: Binance::default(),
            pub_key:"".to_string(),
            sec_key:"".to_string(),
            current_order_id:0,
            ws_buffer_size:50,
            ws_tick: WSTick::default(),
            account_info:None,
        }
    }
}

impl BinanceClient {
    pub fn new(pub_key: String, sec_key: String, collect:Arc<SMutex<HashMap<String,SymbolOutput>>>, live_price_watch:Arc<Mutex<f64>>, live_price_send:Sender<f64>) -> Self {
        Self {
            binance_client: Binance::with_key_and_secret(&pub_key, &sec_key),
            pub_key: pub_key,
            sec_key: sec_key,
            current_order_id: 0,
            ws_buffer_size: 15,
            ws_tick: WSTick::new(collect, live_price_watch, live_price_send),
            account_info: None,
        }
    }
    #[instrument(level="debug")]
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
    #[instrument(level="debug")]
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
    #[instrument(level="debug")]
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
    #[instrument(level="debug")]
    pub async fn connect_ws(
        &mut self,
        params: HashMap<String, Vec<String>>,
    ) -> Result<()> {
        //NOTE may have to get rid this self due to inter mutability,,,
        self.ws_tick.sub_params = params;
        self.ws_tick
            .run(self.ws_buffer_size)
            .await
            .context(err_ctx)?;
        tracing::trace!["Connect WS ws: {:?}",&self];
        Ok(())
    }
    #[instrument(level="debug")]
    async fn change_ws_params(
        &mut self,
        new_params: HashMap<String, Vec<String>>,
    ) -> Result<()> {
        //TODO check if possible to change params without disconnecting
        self.ws_tick.disconnect = true;
        self.ws_tick.sub_params = new_params;
        self.ws_tick
            .run(self.ws_buffer_size)
            .await
            .context(err_ctx)?;
        tracing::debug!["{:?}",&self];
        Ok(())
    }
    #[instrument(level="debug")]
    pub async fn disconnect_ws(&mut self) {
        tracing::debug!["{:?}",&self];
        self.ws_tick.disconnect = true;
        tracing::debug!["{:?}",&self];
    }
    #[instrument(level="debug")]
    pub async fn get_user_data(&mut self) -> Result<()> {
        let resp = self
            .binance_client
            .request(GetAccountRequest {})
            .await
            .context(err_ctx)?;
        tracing::debug!["{:?}",&self];
        log::info!("{:?}", resp);
        self.account_info = Some(resp);
        Ok(())
    }
    #[instrument(level="debug")]
    pub async fn parse_binance_instructs(
        &mut self,
        i: BinInstructs,
    ) -> BinResponse {
        //TODO query state here...
        match i {
            BinInstructs::ConnectWS { params: ref p } => {
                tracing::debug!["Connect ws start {:?}",&self];
                let res=self.connect_ws(p.clone()).await;
                let resp:BinResponse=match res {
                    Ok(_) => {
                        BinResponse::Success
                    }
                    Err(e) => {
                        let string_error=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} Unable to connect to ws  e:{}", i.clone(), e.context(err_ctx)]
                        );
                        //TODO check internet connection here with max retryies
                        //BinResponse::Failure((string_error,GeneralError::SystemError(Sys_err::No_Network)))
                        BinResponse::Failure((string_error,GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}",&self];
                resp
            }
            BinInstructs::ConnectUserWS { params: ref p } => {
                tracing::debug!["Connect ws start {:?}",&self];
                let res = self.start_user_stream().await;
                let resp:BinResponse=match res {
                    Ok(_) => {
                        BinResponse::Success
                    }
                    Err(e) => {
                        let string_error=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(err_ctx)]
                        );
                        //TODO check internet connection here with max retryies
                        //BinResponse::Failure((string_error,GeneralError::SystemError(Sys_err::No_Network)))
                        BinResponse::Failure((string_error,GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}",&self];
                resp
            }
            BinInstructs::Disconnect => {
                let res = self.disconnect_ws().await;
                BinResponse::Success
            }
            BinInstructs::GetUserData => {
                tracing::debug!["Connect ws start {:?}",&self];
                let res = self.get_user_data().await;
                let resp=match res {
                    Ok(_) => {
                        BinResponse::Success
                    }
                    Err(e) => {
                        let string_error=format!["{}",e];
                        log::error!("{}", anyhow!["{:?} Unable to  e:{}", i, e]);
                        BinResponse::Failure((string_error,GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}",&self];
                resp
            }
            BinInstructs::PlaceOrder {
                symbol: ref s,
                o: order,
            } => {
                tracing::debug!["Connect ws start {:?}",&self];
                let (order_type, order_side, price, stop_price, quantity) =
                    parse_order_to_ba(&order);
                let res = self
                    .send_new_order(&s, order_type, order_side, price, quantity)
                    .await;
                let resp=match res {
                    Ok(_) => {
                        BinResponse::Success
                    }
                    Err(e) => {
                        let string_error=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(err_ctx)]
                        );
                        BinResponse::Failure((string_error,GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}",&self];
                resp
            }
            BinInstructs::CancelAndReplaceOrder {
                symbol: ref s,
                o: order,
            } => {
                tracing::debug!["Connect ws start {:?}",&self];
                let res = self.cancel_order(&s).await;
                match res {
                    Ok(_) => {
                    }
                    Err(e) => {
                        let string_error=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(err_ctx)]
                        );
                        return BinResponse::Failure((string_error,GeneralError::Generic));
                    }
                };
                let (order_type, order_side, price, stop_price, quantity) =
                    parse_order_to_ba(&order);
                let res = self
                    .send_new_order(&s, order_type, order_side, price, quantity)
                    .await;
                let resp=match res {
                    Ok(_) => {
                        BinResponse::Success
                    }
                    Err(e) => {
                        let string_error=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} Unable to  e:{}", i.clone(), e.context(err_ctx)]
                        );
                        BinResponse::Failure((string_error,GeneralError::Generic))
                    }
                };
                tracing::debug!["Connect ws end {:?}",&self];
                resp
            }
            BinInstructs::CancelAllOrders { symbol: ref s } => {
                //TODO rewrite this to support multiple orders
                let res = self.cancel_order(&s).await;
                let resp=match res {
                    Ok(_) => {
                        BinResponse::Success
                    }
                    Err(e) => {
                        let string_error=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow![
                                "{:?} Unable to sell_now e:{}",
                                i.clone(),
                                e.context(err_ctx)
                            ]
                        );
                        BinResponse::Failure((string_error,GeneralError::Generic))
                    }
                };
                resp
            }
            BinInstructs::None => BinResponse::None,
        }
    }
}
//TODO AnyhowContext one level above
#[instrument(level="debug")]
fn parse_order_to_ba(
    order: &Order,
) -> (OrderType, OrderSide, f64, f64, f64) {
    //async fn send_new_order(&self, sym:String, otype: OrderType, sid:OrderSide, pric:f64, quant:f64)
    tracing::debug!["Order to parse:{:?}",&order];
    let res: (OrderType, bool, f64, f64, f64) = match order {
        Order::Market { buy: b, quant: q } => {
            (OrderType::Market, *b, 0.0, 0.0, q.get_f64())
        }
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
        Order::None=>{
            panic!("Order none should never be parsed!")
        }
    };
    let side;
    if res.1 == true {
        side = OrderSide::Buy;
    } else {
        side = OrderSide::Sell;
    }
    tracing::debug!["End order parsed:{:?}",&res];
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
    async fn init_ws()->Result<()> {
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
