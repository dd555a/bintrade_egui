use crate::data::{AssetData, Intv, Kline};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use std::result::Result as OCResult;
use tracing::instrument;

const fn exec_order(
    asset1: f64,
    asset2: f64,
    p: f64,
    quant: f64,
    buy_sell: bool,
    fee: f64,
) -> (f64, f64) {
    if buy_sell == true {
        let asset1 = (asset2 / p) * quant * (1.0 - fee);
        let asset2 = asset1 * p * (1.0 - quant);
        (asset1, asset2)
    } else {
        let asset2 = (asset1 * p) * quant * (1.0 - fee);
        let asset1 = (asset2 / p) * (1.0 - quant);
        (asset1, asset2)
    }
}

const fn eval_stop(stop: f64, h: f64, o: f64, _c: f64, l: f64) -> Option<f64> {
    if o > stop {
        if l <= stop {
            return Some(l);
        } else {
            return None;
        }
    }
    if o == stop {
        return Some(o);
    } else {
        if h >= stop {
            return Some(h);
        } else {
            return None;
        }
    }
}

const fn eval_limit(
    limit: f64,
    h: f64,
    o: f64,
    c: f64,
    l: f64,
    buy_sell: bool,
    eval_mode_var: i32,
) -> Option<f64> {
    if eval_mode == 1 {
        if o > c {
            let _h = o;
            let _l = c;
        } else {
            let _h = c;
            let _l = o;
        }
    }
    if buy_sell == true {
        if l <= limit {
            return Some(limit);
        } else {
            return None;
        }
    } else {
        if h >= limit {
            return Some(limit);
        } else {
            return None;
        }
    }
}

pub enum OrderCondition {
    Untouched,
    Filled,
    StopTriggered,
}

const eval_mode: i32 = 1;
const m_fee: f64 = 0.0015;
const t_fee: f64 = 0.0015;
//NOTE - const fucntions for default fallbacks...
pub const fn eval_order_basic(
    h: f64,
    o: f64,
    c: f64,
    l: f64,
    asset1: f64,
    asset2: f64,
    order: Order,
) -> Option<(OrderCondition, f64, f64, f64)> {
    let last_order_price;
    match order {
        Order::Market { buy: b, quant: q } => {
            let quant = q.get_f64();
            let buy_sell = b;
            let (asset1, asset2) = exec_order(asset1, asset2, o, quant, buy_sell, t_fee);
            let condition = OrderCondition::Filled;
            last_order_price = o;
            Some((condition, asset1, asset2, last_order_price))
        }
        Order::Limit {
            buy: b,
            price: p,
            limit_status: li,
            quant: q,
        } => {
            let quant = q.get_f64();
            let buy_sell = b;
            let limit = p;
            let order = eval_limit(limit, h, o, c, l, buy_sell, eval_mode);
            match order {
                Some(price) => {
                    let (asset1, asset2) =
                        exec_order(asset1, asset2, price, quant, buy_sell, m_fee);
                    let condition = OrderCondition::Filled;
                    last_order_price = o;
                    Some((condition, asset1, asset2, last_order_price))
                }
                None => None,
            }
        }
        Order::StopLimit {
            buy: b,
            price: p,
            stop_price: sp,
            stop_status: s,
            limit_status: li,
            quant: q,
        } => {
            let quant = q.get_f64();
            let buy_sell = b;
            let limit = p;
            let stop: f64 = p * (sp as f64);
            //NOTE sp is key1 and multiplies the price, sp 1.1...= 10% above price -- for keybind
            //order change
            let order = eval_stop(stop, h, o, c, l);
            match order {
                Some(price) => {
                    let (asset1, asset2) =
                        exec_order(asset1, asset2, price, quant, buy_sell, m_fee);
                    let condition = OrderCondition::StopTriggered;
                    let limit_order = eval_limit(limit, h, o, c, l, buy_sell, eval_mode);
                    match limit_order {
                        Some(price) => {
                            let (asset1, asset2) =
                                exec_order(asset1, asset2, price, quant, buy_sell, m_fee);
                            let condition = OrderCondition::Filled;
                            last_order_price = o;
                            Some((condition, asset1, asset2, last_order_price))
                        }
                        None => {
                            last_order_price = o;
                            Some((condition, asset1, asset2, last_order_price))
                        }
                    }
                }
                None => None,
            }
        }
        Order::StopMarket {
            buy: b,
            price: p,
            stop_status: s,
            quant: q,
        } => {
            let quant = q.get_f64();
            let buy_sell = b;
            let stop = p;
            let order = eval_stop(stop, h, o, c, l);
            match order {
                Some(price) => {
                    let (asset1, asset2) =
                        exec_order(asset1, asset2, price, quant, buy_sell, m_fee);
                    let condition = OrderCondition::Filled;
                    last_order_price = o;
                    Some((condition, asset1, asset2, last_order_price))
                }
                None => None,
            }
        }
        _ => panic!(),
    }
}

pub enum BasicOrderType {
    Market,
    Limit,
    StopLimit,
    StopMarket,
}

pub const fn eval_basic_condition(
    condition: OrderCondition,
    order_type: Order,
) -> (OrderCondition, Order) {
    match order_type {
        Order::Market { buy: b, quant: q } => match condition {
            OrderCondition::Untouched => panic!("Invalid order state!"),
            OrderCondition::Filled => (OrderCondition::Filled, Order::Market { buy: b, quant: q }),
            OrderCondition::StopTriggered => panic!("Invalid order state!"),
        },
        Order::Limit {
            buy: b,
            price: p,
            limit_status: l,
            quant: q,
        } => match condition {
            OrderCondition::Untouched => (
                OrderCondition::Untouched,
                Order::Limit {
                    buy: b,
                    price: p,
                    limit_status: l,
                    quant: q,
                },
            ),
            OrderCondition::Filled => (
                OrderCondition::Filled,
                Order::Limit {
                    buy: b,
                    price: p,
                    limit_status: l,
                    quant: q,
                },
            ),
            OrderCondition::StopTriggered => panic!("Invalid order state!"),
        },
        Order::StopLimit {
            buy: b,
            price: p,
            stop_price: sp,
            stop_status: s,
            limit_status: l,
            quant: q,
        } => match condition {
            OrderCondition::Untouched => (
                OrderCondition::Untouched,
                Order::StopLimit {
                    buy: b,
                    price: p,
                    stop_price: sp,
                    stop_status: s,
                    limit_status: l,
                    quant: q,
                },
            ),
            OrderCondition::Filled => (
                OrderCondition::Filled,
                Order::StopLimit {
                    buy: b,
                    price: p,
                    stop_price: sp,
                    stop_status: s,
                    limit_status: l,
                    quant: q,
                },
            ),
            OrderCondition::StopTriggered => (
                OrderCondition::Untouched,
                Order::Limit {
                    buy: b,
                    price: p,
                    limit_status: l,
                    quant: q,
                },
            ),
        },
        Order::StopMarket {
            buy: b,
            price: p,
            stop_status: s,
            quant: q,
        } => match condition {
            OrderCondition::Untouched => (
                OrderCondition::Untouched,
                Order::StopMarket {
                    buy: b,
                    price: p,
                    stop_status: s,
                    quant: q,
                },
            ),
            OrderCondition::Filled => (
                OrderCondition::Filled,
                Order::StopMarket {
                    buy: b,
                    price: p,
                    stop_status: s,
                    quant: q,
                },
            ),
            OrderCondition::StopTriggered => panic!("Invalid order condition!"),
        },
        _ => panic!(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LimitStatus {
    Untouched,
    PartFilled { percent_fill: f32 },
    FullyFilled,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StopStatus {
    Untouched,
    Triggered,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Quant {
    Q100,
    Q75,
    Q50,
    Q25,
    Q { q: f64 },
}
impl Quant {
    pub const fn get_f64(&self) -> f64 {
        match self {
            Quant::Q100 => 1.0,
            Quant::Q75 => 0.75,
            Quant::Q50 => 0.5,
            Quant::Q25 => 0.25,
            Quant::Q { q: qq } => *qq,
        }
    }
    pub const fn from_f64(qq: f64) -> Self {
        Quant::Q { q: qq }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Order {
    #[default]
    None,
    Market {
        buy: bool,
        quant: Quant,
    },
    Limit {
        buy: bool,
        quant: Quant,
        price: f64,
        limit_status: LimitStatus,
    },
    StopLimit {
        buy: bool,
        quant: Quant,
        price: f64,
        limit_status: LimitStatus,
        stop_price: f32,
        stop_status: StopStatus,
    },
    StopMarket {
        buy: bool,
        quant: Quant,
        price: f64,
        stop_status: StopStatus,
    },
}
impl Order {
    pub fn get_side(&self) -> bool {
        match &self {
            Order::None => panic!["None order should never have this called on it"],
            Order::Market { buy: b, quant: _ } => return b.clone(),
            Order::Limit {
                buy: b,
                quant: _,
                price: _,
                limit_status: _,
            } => return b.clone(),
            Order::StopLimit {
                buy: b,
                quant: _,
                price: _,
                limit_status: _,
                stop_price: _,
                stop_status: _,
            } => return b.clone(),
            Order::StopMarket {
                buy: b,
                quant: _,
                price: _,
                stop_status: _,
            } => return b.clone(),
        }
    }
    pub fn get_side_str(&self) -> String {
        let side = self.get_side();
        if side == true {
            return "BUY".to_string();
        } else {
            return "SELL".to_string();
        }
    }
    pub fn get_qnt(&self) -> f64 {
        match &self {
            Order::None => panic!("None Order has no side..."),
            Order::Market { buy: _, quant: q } => return q.get_f64(),
            Order::Limit {
                buy: _,
                quant: q,
                price: _,
                limit_status: _,
            } => return q.get_f64(),
            Order::StopLimit {
                buy: _,
                quant: q,
                price: _,
                limit_status: _,
                stop_price: _,
                stop_status: _,
            } => return q.get_f64(),
            Order::StopMarket {
                buy: _,
                quant: q,
                price: _,
                stop_status: _,
            } => return q.get_f64(),
        }
    }
    pub fn get_price(&self) -> &f64 {
        match &self {
            Order::None => panic!("None Order has no side..."),
            Order::Market { buy: _, quant: _ } => &0.0,
            Order::Limit {
                buy: _,
                quant: _,
                price: p,
                limit_status: _,
            } => return p,
            Order::StopLimit {
                buy: _,
                quant: _,
                price: p,
                limit_status: _,
                stop_price: _,
                stop_status: _,
            } => return p,
            Order::StopMarket {
                buy: _,
                quant: _, //Qua
                price: p,
                stop_status: _,
            } => return p,
        }
    }
    pub fn to_str(&self) -> String {
        match &self {
            Order::None => "".to_string(),
            Order::Market { buy: _, quant: _ } => "Market".to_string(),
            Order::Limit {
                buy: _,
                quant: _,
                price: p,
                limit_status: _,
            } => "Limit".to_string(),
            Order::StopLimit {
                buy: _,
                quant: _,
                price: p,
                limit_status: _,
                stop_price: _,
                stop_status: _,
            } => "Stop Limit".to_string(),
            Order::StopMarket {
                buy: _,
                quant: _,
                price: p,
                stop_status: _,
            } => "Stop Market".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AdvOrder {
    TrailingStop,
    OcoStopMarket {
        buy: bool,
        limit_price: f64,
        stop_price: f32,
        limit_stats: LimitStatus,
        stop_status: StopStatus,
    },
    OcoStopLimit {
        buy: bool,
        limit1_price: f64,
        stop_price: f32,
        limit2_price: f32,
    },
}

#[derive(Debug, Clone, Copy)]
enum OrderStatus {
    BasicOrder { t: Order },
    AdvOrder { t: AdvOrder },
}

#[instrument(level = "debug")]
fn hist_eval_kline(
    kline: &[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)],
    mut order: Order,
    asset1: f64,
    asset2: f64,
) -> Option<(chrono::NaiveDateTime, f64, f64, Order)> {
    //TODO get this working then fix the performance bullshit with the .clone()
    for k in kline.iter() {
        let (t, o, h, l, c, _) = *k;
        let result = eval_order_basic(h, o, c, l, asset1, asset2, order);
        match result {
            Some((mut order_cond, asset1, asset2, last_price)) => {
                let (order_cond, order) = eval_basic_condition(order_cond, order);
                match order_cond {
                    OrderCondition::Untouched => continue,
                    _ => return Some((t, asset1, asset2, order)),
                }
            }
            None => continue,
        }
    }
    return None;
}

#[derive(Clone, Debug)]
pub struct HistTrade {
    pub asset_pair: String,
    pub asset1: f64,
    pub asset2: f64,

    pub last_ch_a1: f32,
    pub last_ch_a2: f32,

    pub trades_made: i32,
    pub asset1_held: bool,
    pub last_asset1: f64,
    pub last_asset2: f64,
    pub ch1: f64,
    pub ch2: f64,

    pub start_time: i64,
    pub trade_time: i64,

    pub current_intv: Intv,

    pub trade_record: Vec<TradeRecord>,

    pub current_data_end_index: usize,

    pub current_index: usize,
    asset_data: Arc<Mutex<AssetData>>,
    current_order: Option<Order>,
}
impl Default for HistTrade {
    fn default() -> Self {
        Self {
            asset_pair: "BTCUSDT".to_string(),
            start_time:0,
            asset_data: Arc::new(Mutex::new(AssetData::new(666))),

            asset1: 10_000.0,
            asset2: 0.0,
            last_ch_a1: 0.0,
            last_ch_a2: 0.0,

            trades_made: 0,
            asset1_held: false,
            last_asset1: 0.0,
            last_asset2: 0.0,
            ch1: 0.0,
            ch2: 0.0,

            trade_time: 0,
            trade_record: vec![],
            current_intv: Intv::Min15,
            current_data_end_index: 0,
            current_index: 0,
            current_order: None,
        }
    }
}
impl HistTrade {
    pub fn new(asset_data: Arc<Mutex<AssetData>>) -> Self {
        Self {
            asset_data,
            ..Default::default()
        }
    }
    pub fn place_order(&mut self, order: &Order) {
        self.current_order = Some(*order);
    }
    pub fn start(
        asset_pair: String,
        start_time: i64,
        asset_data: Arc<Mutex<AssetData>>,
    ) -> Self {
        Self {
            asset_pair,
            start_time,
            asset_data,
            ..Default::default()
        }
    }
    pub fn get_trade_time(&mut self) {
        let timestep = self.current_intv.to_timedelta();
    }
    pub fn trade_forward(&mut self, next_wicks: u16) -> Result<()>{
        let ad = Arc::clone(&self.asset_data);
        let asset_data = ad.lock().expect("(TRADE) ad mutex poisoned");
        let trade_slice = asset_data.find_slice_n(
            &self.asset_pair,
            &self.current_intv,
            &self.trade_time,
            next_wicks,
        );
        let ts = match trade_slice {
            Some(t) => {
                tracing::debug!["Trade slice start time {}", t[0].0];
                t
            }
            None => {
                log::error!["Trade slice not found"];
                return Ok(());
            }
        };
        let o: Order;
        match self.current_order {
            Some(oo) => o = oo,
            None => {
                log::error!["No order set"];
                return Ok(());
            }
        }
        tracing::debug!["HistTradingRunner {:?}", self.trade_time];
        //TODO replace this with index
        let result = hist_eval_kline(ts, o, self.asset1, self.asset2);
        tracing::debug!["Hist_eval_kline result: {:?}", result];
        match result {
            Some((transaction_time, asset1, asset2, order)) => {
                tracing::debug!["Hist_Trade_Forward. Transaction Time: {}\n Asset1: {}\n Asset2: {} \n Order: {:?}", transaction_time, asset1, asset2, order];
                let tr = TradeRecord {
                    transaction_time,
                    trades_made: self.trades_made,
                    asset1_held: self.asset1_held,
                    asset1: self.asset1,
                    asset2: self.asset2,
                    last_asset1: self.last_asset1,
                    last_asset2: self.last_asset2,
                    ch1: self.ch1,
                    ch2: self.ch1,
                };
                self.trade_record.push(tr);
                self.asset1 = asset1;
                self.asset2 = asset2;
                match order {
                    Order::None => return Ok(()),
                    _ => 
                    {
                        self.current_order = Some(order);
                        return Ok(());
                    }
                }
            }
            None => return Ok(()),
        }
    }
    pub fn calculate_change(&mut self) {
        self.trades_made += 1;
        if self.trades_made > 1 {
            if !self.asset1_held {
                match self.asset2 {
                    0.0 => {}
                    _ => {
                        let c = (self.asset2 / self.last_asset2) - 1.0;
                        tracing::debug!["{:?}", c];
                        self.ch2 = 100.0 * c;
                        self.last_asset2 = self.asset2;
                        tracing::debug!["C:{:?}, CH1:{:?}, Asset1:{:?}", c, self.ch2, self.asset2];
                    }
                }
            } else {
                match self.asset1 {
                    0.0 => {}
                    _ => {
                        let c = (self.asset1 / self.last_asset1) - 1.0;
                        self.ch2 = 100.0 * c;
                        self.last_asset1 = self.asset1;
                        tracing::debug!["C:{:?}, CH2:{:?}, Asset1:{:?}", c, self.ch1, self.asset1];
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
struct TradeLive {
    trade_start_time: chrono::NaiveDateTime,
    remote_server: bool,

    trade_record: Vec<TradeRecord>,
    set_last_order: bool,
    current_symbol: String,
    s1: f64,
    s2: f64,
    current_interval: String,
    trade_platform: i32,
}

#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize)]
struct TradeRecord {
    transaction_time: chrono::NaiveDateTime,
    trades_made: i32,
    asset1_held: bool,
    asset1: f64,
    asset2: f64,
    last_asset1: f64,
    last_asset2: f64,
    ch1: f64,
    ch2: f64,
}

impl TradeRecord {
    fn new() -> Self {
        TradeRecord {
            transaction_time: chrono::NaiveDateTime::default(),
            trades_made: 0,
            asset1_held: false,
            asset1: 0.0,
            asset2: 0.0,
            last_asset1: 0.0,
            last_asset2: 0.0,
            ch1: 0.0,
            ch2: 0.0,
        }
    }
}
