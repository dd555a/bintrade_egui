use chrono::{
    DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone,
    Utc,
};
use serde::{Deserialize, Serialize};
use std::{result::Result as OCResult,collections::HashMap,env,path::Path, str::FromStr};

use std::sync::{Arc, Mutex};
use time::{OffsetDateTime, macros::datetime};

use strum::IntoEnumIterator;
use strum_macros::EnumIter;

use anyhow::{Context, Result, anyhow};

use crate::{SQLInstructs,SQLResponse,GeneralError};

use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::watch::{Receiver, Sender};
use tokio::time::{Duration, sleep};

use sqlx::{
    Error, Execute, Pool, QueryBuilder, Sqlite, SqlitePool,
    migrate::MigrateDatabase, sqlite::SqliteConnectOptions,
};

use rust_decimal::prelude::ToPrimitive;


//use binance_history::{get_klines,SpotKline};
use binance::api::Binance;
use binance::market::Market;
use binance::model::{KlineSummaries, KlineSummary};


use plotters::prelude::*;

const err_ctx:&str="SQL Data loader";
const metadata_db_path:&str="./metadata.db";

use tracing::instrument;


use yfinance_rs::core::models::Interval;
use yfinance_rs::{Candle, HistoryBuilder, YfClient, YfError};
use yahoo_finance_api as yahoo;



fn yfintv_conv(intv: Intv) -> Interval {
    match intv {
        _ => todo!(),
    }
}

async fn get_yfinance_data(symbol: String, intv: Intv) -> Result<Kline> {
    let client = YfClient::default();
    let interval: Interval = yfintv_conv(intv);
    let hist_download = HistoryBuilder::new(&client, symbol).interval(interval);
    let kl = hist_download.fetch().await?;
    let mut kline: Vec<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)> =
        vec![];
    kl.iter().map(|k| {
        let s = yfcandle_conv(&k);
        match s{
            Ok(slice)=>{kline.push(slice.clone()); Ok(())},
            Err(e)=>Err(e),
        };
    });
    Ok(Kline::new_sql(kline))
}

fn yfcandle_conv(
    input: &Candle,
) -> Result<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)> {
    let time: chrono::NaiveDateTime =
        DateTime::from_timestamp(input.ts, 0).ok_or(anyhow!["Unable to parse time from timestamp"])?.naive_utc();
    let open = input.open;
    let high = input.high;
    let low = input.low;
    let close = input.close;
    let volume: f64;
    match input.volume {
        Some(v) => volume = v as f64,
        None => volume = 0.0,
    }
    Ok((time, open, high, low, close, volume))
}











async fn exec_query(pool: &Pool<Sqlite>, q: &str) -> Result<()> {
    let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(q);
    let query = query_builder.build();
    let result = query.execute(pool).await?;
    log::info!("{:?}", result);
    Ok(())
}


async fn connect_sqlite<P: AsRef<Path>>(filename: P) -> Result<Pool<Sqlite>> {
    let options = SqliteConnectOptions::new()
        .filename(filename)
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePool::connect_with(options).await?;
    Ok(pool)
}
#[instrument(level="debug")]
async fn kfrom_sql(
    pool: &Pool<Sqlite>,
    intv: &str,
    t: Option<(i64, i64)>,
) -> Result<Kline> {
    let q:&str=match t {
        None => {
            log::info!("Load all data:{}", intv);
            &format!(
                "SELECT [Open Time], Open, High, Low, Close, Volume  FROM {};",
                intv
            )
        }
        Some((ts, te)) => {
            log::info!("Load data between {} and {}", ts, te);
            &format!(
                "SELECT [Open Time], Open, High, Low, Close, Volume  FROM {} WHERE {} <= [Timestamp ms] <= {};",
                intv,
                ts,
                te,
            )
        }
    };
    let k: Vec<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)> =
        sqlx::query_as(
            q
        )
        .fetch_all(pool)
        .await?;
    let kline = Kline::new_sql(k);
    Ok(kline)
}

#[instrument(level="debug")]
async fn get_sql_timestamps(
    pool: &Pool<Sqlite>,
    table: &str,
) -> Result<(chrono::NaiveDateTime, chrono::NaiveDateTime, usize)> {
    //NOTE THIS IS UTTERLY FUCKING RETARDED ... but it works for now...
    let q1 = format!(
        "
    SELECT LAST([Timestamp MS], [Open Time], Open, High, Low, Close, Volume) AS output 
    FROM {};",
        table
    );
    let q2 = format!(
        "
    SELECT FIRST([Timestamp MS], [Open Time], Open, High, Low, Close, Volume) AS output 
    FROM {};",
        table
    );
    let qc = format!(
        "
    SELECT COUNT(*)
    FROM {};",
        table
    );
    let start: (chrono::NaiveDateTime, f64, f64, f64, f64, f64) =
        sqlx::query_as(&q2)
            .fetch_one(pool)
            .await
            .expect("Could not fetch first row");
    let end: (chrono::NaiveDateTime, f64, f64, f64, f64, f64) =
        sqlx::query_as(&q1)
            .fetch_one(pool)
            .await
            .expect("Could not fetch last row");
    let count: (u64,) = sqlx::query_as(&qc)
        .fetch_one(pool)
        .await
        .expect("Could not fetch row count");
    let start_time = start.0;
    let end_time = end.0;
    let (c,) = count;
    return Ok((start_time, end_time, c as usize));
}

async fn append_kline(
    pool: &Pool<Sqlite>,
    table: &str,
    input: &[(
        i64,
        chrono::NaiveDateTime,
        f64,
        f64,
        f64,
        f64,
        f64,
        i64,
        chrono::NaiveDateTime,
        f64,
        u64,
        f64,
        f64,
    )],
) -> Result<()> {
    let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
        "INSERT OR REPLACE INTO {}([Timestamp MS] INTEGER,[Open Time], Open, High, Low, Close, Volume, [Close Timestamp MS], [Close Time], [Quote Asset Volume], [Number of Trades], [Taker Buy Base Asset Volume], [Taker Buy Quote Asset Volume] ) ",
        table
    ));
    assert!(input.len() <= 65535 / 11);
    query_builder.push_values(
        input.iter(),
        |mut b, (tt, t, o, h, l, c, v,ctt, ct, qav, no, qbbav, abtav)| {
            b.push_bind(t)
                .push_bind(tt)
                .push_bind(o)
                .push_bind(h)
                .push_bind(l)
                .push_bind(c)
                .push_bind(v)
                .push_bind(ctt)
                .push_bind(ct)
                .push_bind(qav)
                .push_bind(no.clone() as i64)
                .push_bind(qbbav)
                .push_bind(abtav);
        },
    );
    let mut query = query_builder.build();
    let result = query.execute(pool).await?;
    log::info!("{:?}", result);
    Ok(())
}

//TODO -- Iterativelly download and append shit just like this function
async fn sql_append_kline_iter(
    input: &FatKline,
    intv: &Intv,
    iter_size: &usize,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let no_steps = (input.kline.len() - 1) % *iter_size;
    let last_step = no_steps * (*iter_size);
    let table_name = format!("kline_{}", intv.to_str());
    for i in (0..no_steps) {
        append_kline(
            pool,
            &table_name,
            &input.kline[iter_size * i..iter_size * (i + 1)],
        )
        .await?;
    }
    append_kline(pool, &table_name, &input.kline[last_step..]).await?;
    Ok(())
}

async fn check_append_kline(
    pool: &Pool<Sqlite>,
    intv: &Intv,
    input: &[(
        i64,
        chrono::NaiveDateTime,
        f64,
        f64,
        f64,
        f64,
        f64,
        i64,
        chrono::NaiveDateTime,
        f64,
        u64,
        f64,
        f64,
    )],
) -> Result<()> {
    let is_time = input[0].1;
    let ie_time = input[input.len() - 1].1;
    let table_name = format!("kline_{}", intv.to_str());
    let (os_time, oe_time, no_rows) =
        get_sql_timestamps(pool, &table_name).await?;
    let check_time = is_time + intv.to_timedelta();
    if check_time == oe_time {
        //append normally
        append_kline(pool, &table_name, input).await;
    } else {
        if oe_time >= is_time {
            //remove the overlap then append
            let overlap_points = ((oe_time - is_time).num_minutes() as usize)
                / (intv.to_min() as usize);
            append_kline(pool, &table_name, &input[overlap_points..]).await?;
            log::info!("Data overlapping, pruning")
        } else {
            //Do nothing as there would be gaps in the data
            log::error!("Unable to append data, appending would cause gaps!")
        }
    } //check that kline append start time is 1 higher than sql time to ensure there are no gaps
    Ok(())
}
















#[derive(EnumIter, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intv {
    Min1,
    Min3,
    Min5,
    Min15,
    Min30,
    Hour1,
    Hour2,
    Hour4,
    Hour6,
    Hour8,
    Hour12,
    Day1,
    Day3,
    Week1,
    Month1,
}
impl Intv {
    pub fn from_str(input:&str) -> Self {
        match input {
           "1min"    => Intv::Min1   ,
           "3min"    => Intv::Min3   ,
           "5min"    => Intv::Min5   ,
           "15min"   => Intv::Min15  ,
           "30min"   => Intv::Min30  ,
           "1hour"   => Intv::Hour1  ,
           "2hour"   => Intv::Hour2  ,
           "4hour"   => Intv::Hour4  ,
           "6hour"   => Intv::Hour6  ,
           "8hour"   => Intv::Hour8  ,
           "12hour"  => Intv::Hour12 ,
           "1day"    => Intv::Day1   ,
           "3day"    => Intv::Day3   ,
           "1week"   => Intv::Week1  ,
           "1month"  => Intv::Month1 ,
           _=> panic!("Invalid interval string, Intv::from_str()")
        }
    }
    pub fn to_str(&self) -> &str {
        match &self {
            Intv::Min1 => "1min",
            Intv::Min3 => "3min",
            Intv::Min5 => "5min",
            Intv::Min15 => "15min",
            Intv::Min30 => "30min",
            Intv::Hour1 => "1hour",
            Intv::Hour2 => "2hour",
            Intv::Hour4 => "4hour",
            Intv::Hour6 => "6hour",
            Intv::Hour8 => "8hour",
            Intv::Hour12 => "12hour",
            Intv::Day1 => "1day",
            Intv::Day3 => "3day",
            Intv::Week1 => "1week",
            Intv::Month1 => "1month",
        }
    }
    pub fn to_bin_str(&self) -> &str {
        match &self {
            Intv::Min1 => "1m",
            Intv::Min3 => "3m",
            Intv::Min5 => "5m",
            Intv::Min15 => "15m",
            Intv::Min30 => "30m",
            Intv::Hour1 => "1h",
            Intv::Hour2 => "2h",
            Intv::Hour4 => "4h",
            Intv::Hour6 => "6h",
            Intv::Hour8 => "8h",
            Intv::Hour12 => "12h",
            Intv::Day1 => "1d",
            Intv::Day3 => "3d",
            Intv::Week1 => "1w",
            Intv::Month1 => "1M",
        }
    }
    pub fn from_bin_str(input:&str) -> Self{
        match input {
            "1m"  => Intv::Min1    ,
            "3m"  => Intv::Min3    ,
            "5m"  => Intv::Min5    ,
            "15m" => Intv::Min15   ,
            "30m" => Intv::Min30   ,
            "1h"  => Intv::Hour1   ,
            "2h"  => Intv::Hour2   ,
            "4h"  => Intv::Hour4   ,
            "6h"  => Intv::Hour6   ,
            "8h"  => Intv::Hour8   ,
            "12h" => Intv::Hour12  ,
            "1d"  => Intv::Day1    ,
            "3d"  => Intv::Day3    ,
            "1w"  => Intv::Week1   ,
            "1M"  => Intv::Month1  ,
            _ => panic!("Invalid string parsed Intv::from_bin_str()"),
        }
    }
    pub fn to_timedelta(&self) -> chrono::TimeDelta {
        match &self {
            Intv::Min1 => chrono::TimeDelta::minutes(1),
            Intv::Min3 => chrono::TimeDelta::minutes(3),
            Intv::Min5 => chrono::TimeDelta::minutes(5),
            Intv::Min15 => chrono::TimeDelta::minutes(15),
            Intv::Min30 => chrono::TimeDelta::minutes(30),
            Intv::Hour1 => chrono::TimeDelta::hours(1),
            Intv::Hour2 => chrono::TimeDelta::hours(2),
            Intv::Hour4 => chrono::TimeDelta::hours(4),
            Intv::Hour6 => chrono::TimeDelta::hours(6),
            Intv::Hour8 => chrono::TimeDelta::hours(8),
            Intv::Hour12 => chrono::TimeDelta::hours(12),
            Intv::Day1 => chrono::TimeDelta::days(1),
            Intv::Day3 => chrono::TimeDelta::days(3),
            Intv::Week1 => chrono::TimeDelta::weeks(1),
            Intv::Month1 => self.month_to_timedelta(DateTime::naive_local(
                &chrono::offset::Utc::now(),
            )),
        }
    }
    pub fn month_to_timedelta(
        &self,
        from_reference: NaiveDateTime,
    ) -> chrono::TimeDelta {
        //NOTE returns timedelta based on the current month...
        fn is_leap_year(year: i32) -> bool {
            return (year % 4 == 0) && (year % 100 != 0 || year % 400 == 0);
        }
        let m = from_reference.month();
        let days = match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if is_leap_year(from_reference.year()) == true {
                    29
                } else {
                    28
                }
            }
            _ => panic!(),
        };
        chrono::TimeDelta::days(days)
    }
    pub fn to_sec(&self) -> i64 {
        match &self {
            Intv::Min1 => 1 * 60,
            Intv::Min3 => 3 * 60,
            Intv::Min5 => 5 * 60,
            Intv::Min15 => 15 * 60,
            Intv::Min30 => 30 * 60,
            Intv::Hour1 => 1 * 60 * 60,
            Intv::Hour2 => 2 * 60 * 60,
            Intv::Hour4 => 4 * 60 * 60,
            Intv::Hour6 => 6 * 60 * 60,
            Intv::Hour8 => 8 * 60 * 60,
            Intv::Hour12 => 12 * 60 * 60,
            Intv::Day1 => 1 * 60 * 60 * 24,
            Intv::Day3 => 3 * 60 * 60 * 24,
            Intv::Week1 => 1 * 60 * 60 * 24 * 7,
            Intv::Month1 => todo!(),
        }
    }
    pub fn to_min(&self) -> u64 {
        match &self {
            Intv::Min1 => 1,
            Intv::Min3 => 3,
            Intv::Min5 => 5,
            Intv::Min15 => 15,
            Intv::Min30 => 30,
            Intv::Hour1 => 1 * 60,
            Intv::Hour2 => 2 * 60,
            Intv::Hour4 => 4 * 60,
            Intv::Hour6 => 6 * 60,
            Intv::Hour8 => 8 * 60,
            Intv::Hour12 => 12 * 60,
            Intv::Day1 => 1 * 60 * 24,
            Intv::Day3 => 3 * 60 * 24,
            Intv::Week1 => 1 * 60 * 24 * 7,
            Intv::Month1 => todo!(),
        }
    }
}


#[derive(Serialize, Deserialize)]
pub struct FatKline {
    //(time o h l c volume,)
    pub kline: Vec<(
        i64,
        chrono::NaiveDateTime,
        f64,
        f64,
        f64,
        f64,
        f64,
        i64,
        chrono::NaiveDateTime,
        f64,
        u64,
        f64,
        f64,
    )>,
}
impl std::fmt::Display for FatKline{
    fn fmt(&self, formatter:&mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error>{
        Ok(())
    }
}
impl std::fmt::Debug for FatKline{
    fn fmt(&self, formatter:&mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error>{
        Ok(())
    }
}
impl FatKline {
    fn new(
        kline: Vec<(
            i64,
            chrono::NaiveDateTime,
            f64,
            f64,
            f64,
            f64,
            f64,
            i64,
            chrono::NaiveDateTime,
            f64,
            u64,
            f64,
            f64,
        )>,
    ) -> Self {
        Self { kline }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Kline {
    //(time o h l c volume)
    pub kline: Vec<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>,
}
impl Kline {
    fn new() -> Self {
        Self {
            kline: std::vec::Vec::new(),
        }
    }
    fn new_sql(
        k: Vec<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>,
    ) -> Self {
        Self { kline: k }
    }
    #[instrument(level="debug")]
    pub fn split(
        &self,
        no_splits: usize,
    ) -> Vec<&[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)]> {
        //XXX TODO switch to chunks
        let batch_size = self.kline.len() % no_splits;
        let output: Vec<&[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)]> =
            (0..no_splits)
                .map(|n| &self.kline[n * &batch_size..n * (&batch_size + 1)])
                .collect();

        return output;
    }
    fn get_start_end(
        &self,
    ) -> Result<(chrono::NaiveDateTime, chrono::NaiveDateTime)> {
        assert_ne!(self.kline.len(), 0);
        let start_time = self.kline[0].0;
        let end_time = self
            .kline
            .last()
            .ok_or(anyhow!["SQL unable to create Kline"])?
            .0;
        Ok((start_time, end_time))
    }
}

#[derive(Debug)]
pub struct Klines {
    //symb:String, //String of the asset etc BTCUSTD
    start_time: chrono::NaiveDateTime, //Data start time
    end_time: chrono::NaiveDateTime,   //Data end time
    full_dat: HashMap<Intv, FatKline>,
    dat: HashMap<Intv, Kline>,
}
impl Klines {
    fn new(
        start_time: chrono::NaiveDateTime,
        end_time: chrono::NaiveDateTime,
        dat: HashMap<Intv, Kline>,
    ) -> Self {
        Self {
            start_time: start_time,
            end_time: end_time,
            full_dat: HashMap::new(),
            dat: dat,
        }
    }
    pub fn new_empty() -> Self {
        let start_time = chrono::NaiveDateTime::from_timestamp(0, 0);
        let end_time = chrono::NaiveDateTime::from_timestamp(0, 0);
        Self {
            end_time,
            start_time,
            full_dat: HashMap::new(),
            dat: HashMap::new(),
        }
    }
    fn insert(&mut self, i: &Intv, kl: Kline) {
        self.dat.insert(i.clone(), kl);
    }
    pub fn insert_fat(&mut self, i: &Intv, kl: FatKline) {
        self.full_dat.insert(i.clone(), kl);
    }
    #[instrument(level="debug")]
    fn get_times2(&self, full: bool) ->Option<(i64,i64)>{
        if full == true {
            let kline = self.full_dat.get(&Intv::Min1);
            match kline {
                Some(kl) => {
                    let start_time = kl.kline[0].1.timestamp_millis();
                    let end_time = kl.kline[kl.kline.len() - 1].1.timestamp_millis();
                    return Some((start_time,end_time));
                }
                None => {
                    log::error!("Unable to get times for kline: {:?}", &self);
                    return None;
                }
            }
        } else {
            let kline = self.dat.get(&Intv::Min1);
            let times=match kline {
                Some(kl) => {
                    let start_time= kl.kline[0].0.timestamp_millis();
                    let end_time = kl.kline[kl.kline.len() - 1].0.timestamp_millis();
                    return Some((start_time,end_time));
                }
                None => {
                    log::error!("Unable to get times for kline: {:?}", &self);
                    return None;
                }
            };
        }
    }
    #[instrument(level="debug")]
    fn get_times(&mut self, full: bool) {
        if full == true {
            let kline = self.full_dat.get(&Intv::Min1);
            match kline {
                Some(kl) => {
                    self.start_time = kl.kline[0].1;
                    self.end_time = kl.kline[kl.kline.len() - 1].1;
                }
                None => {
                    log::error!("Unable to get times for kline: {:?}", &self);
                }
            }
        } else {
            let kline = self.dat.get(&Intv::Min1);
            match kline {
                Some(kl) => {
                    self.start_time= kl.kline[0].0;
                    self.end_time = kl.kline[kl.kline.len() - 1].0;
                }
                None => {
                    log::error!("Unable to get times for kline: {:?}", &self);
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct AssetData {
    id:usize,
    size: usize,
    pub kline_data: HashMap<String, Klines>,

}

impl AssetData {
    pub fn debug(&self)->String{
        let mut ostring:String="Available Hashmaps: \n".to_string();
        if self.kline_data.is_empty() == false{
            ostring.push_str(&format!["  Kline Data id:{} (size):{}\n",self.id ,self.kline_data.len()])
        }else{
            ostring.push_str(&format!["  Kline Data id:{} :EMPTY \n",self.id])
        }
        ostring
    }
    pub fn new(id:usize) -> Self {
        Self {
            id,
            size: 0,
            kline_data: HashMap::new(),
        }
    }
    //#[instrument(level="debug")]
    pub fn load_full_intv(
        &self,
        symbol: &str,
        intv: &Intv,
    ) -> Result<&[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)]> {
        let klines = self
            .kline_data
            .get(symbol)
            .ok_or(anyhow!["Unable to find data for: {}", symbol])?;
        let kline = klines.dat.get(intv).ok_or(anyhow![
            "Unable to find data for: {}, interval: {}",
            symbol,
            intv.to_str()
        ])?;
        Ok(&kline.kline)
    }
    pub fn show_all_loaded(&self) {
        log::info!["All loaded symbols:"];
        self.kline_data.iter().map(|(symbol, _)| {
            log::info!["{}", &symbol];
        });
    }
    //#[instrument(level="debug")]
    pub fn find_slice(
        &self,
        symbol: &str,
        intv: &Intv,
        start_time: &chrono::NaiveDateTime,
        end_time: &chrono::NaiveDateTime,
    ) -> Option<&[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)]> {
        let kk: &Klines;
        let klines = self.kline_data.get(symbol);
        match klines {
            Some(k) => kk = k,
            None => return None,
        }
        if start_time < &kk.start_time {
            return None;
        }
        if end_time > &kk.end_time {
            return None;
        }
        let k: &Kline;
        let kline = kk.dat.get(intv);
        match kline {
            Some(ki) => k = ki,
            None => return None,
        }
        let step: i64 = intv.to_sec();
        let kst = kk.start_time.timestamp();
        let ket = kk.end_time.timestamp();
        let dur = (kst - ket) / step;
        let start_t = start_time.timestamp();
        let end_t = end_time.timestamp();
        let start_index = ((kst - start_t) / dur) as usize;
        let end_index = ((kst - end_t) / dur) as usize;
        return Some(&k.kline[start_index..end_index]);
    }
    #[instrument(level="debug")]
    pub fn find_slice_index(
        &self,
        symbol: &str,
        intv: &Intv,
        start_index: usize,
        end_index: usize,
    ) -> Option<&[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)]> {
        //TODO fix boilerplate maybe
        let kk: &Klines;
        let klines = self.kline_data.get(symbol);
        match klines {
            Some(k) => kk = k,
            None => return None,
        }
        let k: &Kline;
        let kline = kk.dat.get(intv);
        match kline {
            Some(ki) => k = ki,
            None => return None,
        }
        return Some(&k.kline[start_index..end_index]);
    }
    #[instrument(level="debug")]
    pub async fn load_all(
        &mut self,
        symbol: &str,
        sql_pool: &Pool<Sqlite>,
    ) -> Result<()> {
        let search = self
            .kline_data
            .get(symbol)
            .ok_or(anyhow!["Unable to find data for symbol {}", symbol])?;
        let mut klines = Klines::new_empty();
        for intv in Intv::iter() {
            let s: &str = intv.to_str();
            println!("Loading data for:{} interval:{}", symbol, s);
            let k: Kline =
                kfrom_sql(&sql_pool, &format!["kline_{}", &s], None).await?;
            klines.dat.insert(intv, k);
        }
        self.kline_data.insert(symbol.to_string(), klines);
        Ok(())
    }
}









pub fn get_data_binance(
    client: &Market,
    symbol: &str,
    intv: Intv,
    part_download: Option<i64>,
) -> Result<FatKline> {
    #[instrument(level="debug")]
    fn kline_conv(
        input: &KlineSummary,
    ) -> Result<(
        i64,
        chrono::NaiveDateTime,
        f64,
        f64,
        f64,
        f64,
        f64,
        i64,
        chrono::NaiveDateTime,
        f64,
        u64,
        f64,
        f64,
    )> {

        tracing::debug!["Kline conv in = {:?}",&input];
        let based_time=input.open_time;
        let time =
            chrono::NaiveDateTime::from_timestamp_millis(input.open_time)
                .unwrap();
        let open = input.open.parse()?;
        let high = input.high.parse()?;
        let low = input.low.parse()?;
        let close = input.close.parse()?;
        let volume = input.volume.parse()?;
        let based_ctime=input.close_time;
        let ctime =
            chrono::NaiveDateTime::from_timestamp_millis(input.close_time)
                .ok_or(anyhow!["Unable to parse ctime"])?;
        let qav = input.quote_asset_volume.parse()?;
        let no = input.number_of_trades as u64;
        let tbbav = input.taker_buy_base_asset_volume.parse()?;
        let tbqav = input.taker_buy_quote_asset_volume.parse()?;

        let out=(based_time,time, open, high, low, close, volume,based_ctime, ctime, qav, no, tbbav, tbqav,);
        return Ok(
            out
            );
    }
    let res=match part_download{
        Some(timestamp)=>{
            client.get_klines(symbol, intv.to_bin_str(), None, Some(timestamp as u64), None)
        }
        None=>client.get_klines(symbol, intv.to_bin_str(), None, None, None)
    };
    match res {
        Ok(kl_enum) => {
            let klines = match kl_enum {
                KlineSummaries::AllKlineSummaries(a) => a,
            };
            let kl: Result<
                Vec<(
                    i64,
                    chrono::NaiveDateTime,
                    f64,
                    f64,
                    f64,
                    f64,
                    f64,
                    i64,
                    chrono::NaiveDateTime,
                    f64,
                    u64,
                    f64,
                    f64,
                )>,
            > = klines.iter().map(|k| kline_conv(&k)).collect();
            let kline = kl?;
            return Ok(FatKline::new(kline));
        }
        Err(err) => Err(anyhow!("Binance error:{:?}", err)),
    }
}
//TODO test and make functions to load an asset into asset data by downloading it
//

async fn create_metadata_db()->Result<()>{
    create_db(&metadata_db_path)
        .await
        .context(anyhow!("SQL::Unable cereate db"))?;
    let pool = SqlitePool::connect(&metadata_db_path)
        .await
        .context(anyhow!("SQL::Unable to metadata connect to db"))?;
    let q = format!(
        "CREATE TABLE assets ( [Asset] TEXT, [Exchange] TEXT, [Extras] TEXT, [Extras Multi] TEXT, [Start Time ms] INTEGER, [End Time ms] INTEGER, [Market cap] REAL )"
    );
    exec_query(&pool, &q).await?;
    let q=format!(
        "CREATE UNIQUE INDEX Asset
        ON assets ( [Asset] )"
    );
    exec_query(&pool, &q).await?;
    Ok(())
}

async fn load_asset_list(pool:&Pool<Sqlite>)->Result<Vec<(String, String, Option<String>, Option<String>, Option<i64>, Option<i64>)>>{
    let out:Vec<(String,String, Option<String>, Option<String>, Option<i64>,Option<i64>)> = sqlx::query_as(format!("SELECT [Asset], [Exchange], [Extras], [Extras Multi], [Start Time ms], [End Time ms], FROM assets;").as_str())
        .fetch_all(pool)
        .await?;
    Ok(out)
}

async fn update_asset_metadata(metadata_pool:&Pool<Sqlite>, symbol:&str,  start_time:Option<i64>,end_time:i64)->Result<()>{
    if let Some(start_time) = start_time{
        let q = format!(
            "
        REPLACE INTO metadata ( Asset, [Start Time ms], [End Time ms])
        VALUES({}, {}. {});",
        symbol, start_time, end_time
        );
        exec_query(metadata_pool, &q).await?;
    }else{
        let q = format!(
            "
        REPLACE INTO metadata ( Asset, [End Time ms])
        VALUES({}, {});",
        symbol, end_time
        );
        exec_query(metadata_pool, &q).await?;
    };
    Ok(())
}

pub enum Exchange{
    Binance,
    Yahoo
}

impl From<&str> for Exchange{
    fn from(input:&str) -> Self{
        match input{
            "Binance" => Exchange::Binance,
            "Yahoo" => Exchange::Yahoo,
            _=> panic!["Unable to parse exchange name"],
        }
    }
}


async fn get_asset_info(symbol:&str, exchange:&Exchange)->Result<AssetMetadata>{
    match exchange{
        Exchange::Binance=>{
            todo!()
        }
        Exchange::Yahoo=>{
            todo!()
        }
    }
    todo!()
}

async fn download_asset_data(symbol:&str, exchange:&Exchange, timestamp:Option<i64>)->Result<Klines>{
    match exchange{
        Exchange::Binance=>{
            let klines=intv_dl_klines(symbol, timestamp);
        }
        Exchange::Yahoo=>{
            todo!()
        }

    }
    todo!()
}
const ITER_SIZE:usize=4_000;




#[derive(Debug)]
pub struct SQLConn {
    asset_dbs: HashMap<String, Pool<Sqlite>>,
    db_path: String,
    hist_asset_data: Arc<Mutex<AssetData>>,
}
//TODO hist data arc mutex will be stored here. Make a semiphore for max number of data
impl Default for SQLConn {
    fn default() -> Self {
        Self {
            asset_dbs: HashMap::new(),
            db_path: "./src/databases"
                .to_string(),

            hist_asset_data: Arc::new(Mutex::new(AssetData::new(666))),
        }
    }
}

#[instrument(level="debug")]
async fn intv_dl_klines(
    symbol: &str,
    prune: Option<i64>,
) -> Result<Klines> {
    let s = symbol.to_string();
    let klines: Result<Klines, anyhow::Error> =
        tokio::task::spawn_blocking(move || {
            let client: Market = Binance::new(None, None);
            let mut klines = Klines::new_empty();
            let mut res: Result<_, anyhow::Error> = Err(anyhow![""]);
            let mut err_occured = false;
            for i in Intv::iter() {
                let kl = get_data_binance(&client, &s, i, prune)?;
                if kl.kline.is_empty() {
                    err_occured = true;
                    res = Err(anyhow![
                        "Kline empty: {:?}, for interval {}",
                        &kl,
                        i.to_str()
                    ]);
                    break;
                }
                klines.insert_fat(&i, kl);
            }
            if err_occured == false {
                klines.get_times(true);
                Ok(klines)
            } else {
                res
            }
        })
        .await?;
    return klines;
}
async fn create_db(db_path: &str) -> Result<()> {
    if !Sqlite::database_exists(&db_path).await.unwrap_or(false) {
        let result = Sqlite::create_database(&db_path).await?;
        let pool = connect_sqlite(&db_path).await?;
        log::info!("Creating database {:?}", result);
        let q = format!("PRAGMA foreign_keys=ON");
        exec_query(&pool, &q).await?;
        Ok(())
    } else {
        log::info!("Database already exists");
        Ok(())
    }
}


async fn create_kline_tables(
    pool: &Pool<Sqlite>,
    klines: Klines,
    am:AssetMetadata
) -> Result<()> {
    for i in Intv::iter() {
        let q = format!(
            "CREATE TABLE kline_{} ( [Timestamp MS] INTEGER, [Open Time] datetime,  Open REAL,  High REAL,  Low REAL,  Close REAL,  Volume REAL, [Close Timestamp MS] INTEGER, [Close Time] INTEGER,  [Quote Asset Volume] REAL,  [Number of Trades] INTEGER,  [Taker Buy Base Asset Volume] REAL,  [Taker Buy Quote Asset Volume] REAL,[Ignore], [Pattern hash] REAL )",
            i.to_str()
        );
        exec_query(&pool, &q).await?;
        let q=format!(
            "CREATE UNIQUE INDEX time_index
            ON kline_{} ( [Timestamp MS] )",
            i.to_str()
        );
        exec_query(&pool, &q).await?;
        let kline = klines
            .full_dat
            .get(&i)
            .ok_or(anyhow!["Unable to get kline"])?;
        let iter_size;
        if kline.kline.len() < 4_000 {
            iter_size = (kline.kline.len() - 1)
        } else {
            iter_size = 4_000
        };
        sql_append_kline_iter(&kline, &i, &iter_size, &pool).await?;
    }
    let q = format!(
        "CREATE TABLE colstats ( Column TEXT, Type INTEGER, Mean REAL, [Standard Deviaton] , No [INTEGER] )",
    );
    exec_query(&pool, &q).await?;
    let q=format!(
        "CREATE UNIQUE INDEX col_id
        ON colstats ( Column )"
    );
    exec_query(&pool, &q).await?;
    let q = format!(
        "CREATE TABLE metadata (Symbol TEXT, Exchange INTEGER, [Start Time] INTEGER, [End Time] INTEGER, [Key asset class 1] INTEGER, [Sub asset class 1] INTEGER ) "
    );
    exec_query(&pool, &q).await?;
    let q=format!(
        "CREATE UNIQUE INDEX asset_id
        ON metadata ( Symbol )"
    );
    exec_query(&pool, &q).await?;
    update_metadata_time(&pool,am);
    Ok(())
}
//MOVE metadata to a separate generalist db and make a (recreate) function - take the metadata
//table and recreate the database from scratch

enum ColumnTypes{
    KlineDiscrete ,//NOTE the collumn is synced to KLINE with no NIll values
    KlineCont //collumn is is f64
}

#[derive(sqlx::FromRow,Debug)]
struct AssetMetadata{
    symbol:String,
    data_start_ms:i64,
    data_end_ms:i64,

}
/*
    let q = format!(
        "
    SELECT FIRST( Asset, [Start Time], [End Time]) AS output 
    FROM asset ;",
    );
 * */
async fn get_metadata(pool:&Pool<Sqlite>, symbol:&str)->Result<AssetMetadata>{
    let q = format!(
        "SELECT ( Asset, [Start Time ms], [End Time ms]) FROM asset WHERE Asset = {};",symbol
    );
    let am:AssetMetadata =
        sqlx::query_as(&q)
            .fetch_one(pool)
            .await?;
    Ok(am)
}
async fn update_metadata_time(pool:&Pool<Sqlite>,am:AssetMetadata)->Result<()>{
    let q = format!(
        "
    REPLACE INTO metadata (Symbol, [End Time], [Start Time] )
    VALUES({}, {}. {});",
    am.symbol,am.data_start_ms,am.data_end_ms,
    );
    exec_query(&pool, &q).await?;
    Ok(())
}

async fn check_if_columns_exist(pool:&Pool<Sqlite>,columns:&str,table:&str)->Result<bool>{
    let start: (chrono::NaiveDateTime, f64, f64, f64, f64, f64) =
        sqlx::query_as(&format!["PRAGMA table_info({}) WHERE name = {}", columns,table])
            .fetch_one(pool)
            .await?;
    todo!();
}

async fn binance_get_metadata(symbol:&str)->Result<AssetMetadata>{
    todo!()
}
impl SQLConn {
    pub fn new(hist_asset_data: Arc<Mutex<AssetData>>) -> Self {
        Self {
            hist_asset_data,
            ..Default::default()
        }
    }
    pub fn new_testing() -> Self {
        Self::default()
    }
    #[instrument(level="debug")]
    pub async fn dl_binance_asset(
        &self,
        symbol: &str,
        prune: Option<chrono::NaiveDateTime>,
        calc_stats: bool,
    ) -> Result<()> {
        todo!()
    }
    async fn append_binance_asset(symbol: &str) {}
    async fn dl_yahoo_asset(symbol: &str) {}
    async fn append_yahoo_asset(symbol: &str) {}
    //return full asset data struct by iterating over all enums using Strum and getting klines
    #[instrument(level="debug")]
    async fn load_all_data(&mut self, symbol: &str) -> Result<()> {
        let s: String = symbol.to_string();
        let pool =
            connect_sqlite(format!["{}/Asset{}.db", &self.db_path, &symbol])
                .await
                .context("SQL : unable to connect to db")?;

        /*
        let pool=match self.asset_dbs.get(&s){
            Some(p) => p,
            None => {
                //self.asset_dbs.insert(s.to_string(), pool_p);
                &p
            }
        };
        //NOTE this is a hack
         */
        let mut klines = Klines::new_empty();
        for intv in Intv::iter() {
            let s: &str = intv.to_str();
            println!("Loading data for:{} interval:{}", symbol, s);
            let k: Kline = kfrom_sql(&pool, &format!["kline_{}", &s], None)
                .await
                .context("SQL : unable to connect to db")?;
            klines.dat.insert(intv, k);
        }
        let ad_a = Arc::clone(&mut self.hist_asset_data);
        let mut ad = ad_a.lock().expect("Posioned AD mutex! (DATA)");
        let search = ad.kline_data.get(symbol);
        match search {
            Some(_) => {
                return Err(anyhow!["Data for {} already present", symbol]);
            }
            None => ad.kline_data.insert(symbol.to_string(), klines),
        };
        tracing::debug!["Asset Data SQL CONNECT = {}",ad.debug()];
        Ok(())
    }
    #[instrument(level="debug")]
    pub async fn update_data(&self)->Result<()>{
        if Sqlite::database_exists(&metadata_db_path).await.unwrap_or(false) {
            create_metadata_db().await?
        };
        let meta_pool = SqlitePool::connect(&metadata_db_path)
            .await
            .context(anyhow!("SQL::Unable to metadata connect to db"))?;
        let asset_list=load_asset_list(&meta_pool).await?;
        let current_timestamp_ms:i64=match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(n) => {
                let r=n.as_millis().try_into();
                match r{
                    Ok(n)=>Ok(n),
                    Err(e)=>Err(anyhow!["{:?}",e]),
                }
            }
            Err(e) => Err(anyhow!["{:?}",e]),
        }?;
        for ( asset_symbol, exchange, extras, extras_multi, start_time_ms, end_time_ms ) in asset_list {
            let db_path=format!["./databases2/Asset{}.db",asset_symbol];
            if !Sqlite::database_exists(&db_path).await.unwrap_or(false) {
                create_db(&db_path).await;
            };
            let apool = connect_sqlite(format!["{}/AssetList.db", &db_path]).await?;
            let exch = Exchange::from(exchange.as_str());
            let (full_dl,start_time):(bool,i64)=match start_time_ms{
                Some(start_time_ms)=>(true,start_time_ms),
                None =>(false,0),
            };
            let end=end_time_ms.ok_or(anyhow!["START TIME BUT NO END TIME WTF"])?;
            let klines=match full_dl{
                true=>download_asset_data(&asset_symbol, &exch, Some(end)).await?,
                false=>download_asset_data(&asset_symbol, &exch, None).await?,
            };
            let res=klines.get_times2(true);
            let asset_metadata=match res{
                Some((_,end_time))=>{
                    for i in Intv::iter(){
                        let kline=klines.full_dat.get(&i).ok_or(anyhow!["Kline interval not found!"])?;
                        let iter_size;
                        if kline.kline.len() < ITER_SIZE {
                            iter_size = (kline.kline.len() - 1)
                        } else {
                            iter_size = ITER_SIZE
                        };
                        sql_append_kline_iter(&kline, &i, &iter_size, &apool).await?;
                    }
                    Ok(AssetMetadata{symbol:asset_symbol,data_start_ms:start_time,data_end_ms:end_time})
                }
                None => {
                    Err(anyhow!["Unable to get start/end times from kline"])
                }
            }?;
            apool.close();
        };
        meta_pool.close();
        Ok(())
    }
    #[instrument(level="debug")]
    async fn unload_data(&mut self, symbol: &str) -> Result<()> {
        let ad_a = Arc::clone(&mut self.hist_asset_data);
        let mut ad = ad_a.lock().expect("Posioned AD mutex! (DATA)");
        let result = ad.kline_data.get(symbol);
        match result {
            Some(_) => {
                ad.kline_data.remove(symbol);
                Ok(())
            }
            None => return Ok(()),
        }
    }
    async fn insert_ad_intosql(&self, ad: Arc<Mutex<AssetData>>) {}
    #[instrument(level="debug")]
    pub async fn parse_sql_instructs(
        &mut self,
        i: SQLInstructs,
    ) -> SQLResponse {
        match i {
            SQLInstructs::LoadHistData { symbol: ref s } => {
                log::info!("Loading historical data for:{}", s);
                let resp=match self.load_all_data(&s).await {
                    Ok(_) => {
                        SQLResponse::Success
                    }
                    Err(e) => {
                        let err_string=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} SQL::load_all_data : {:?}", i, e.context(err_ctx)]
                        );
                        SQLResponse::Failure((err_string, GeneralError::Generic))
                    }
                };
                resp
            }
            SQLInstructs::UnloadHistData { symbol: ref s } => {
                let res = self.unload_data(&s).await;
                let resp=match res {
                    Ok(_) => {
                         SQLResponse::Success
                    },
                    Err(e) => {
                        let err_string=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} SQL::load_all_data e:{:?}", i, e.context(err_ctx)]
                        );
                        SQLResponse::Failure((err_string, GeneralError::Generic))
                    }
                };
                resp
            }
            SQLInstructs::LoadTradeRecord { id: ref id } => {
                todo!()
            }
            SQLInstructs::None => {
                todo!()
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sql_test() {
        println!("--------------------------------------");
        println!("SQL testing");
        println!("--------------------------------------");
        let mut asset_data = AssetData::new(333);
        let symb: &str = "BTCUSDT";
        let db_path: &str =
            "./src/databases";
        let p = connect_sqlite(format!["{}/Asset{}.db", &db_path, &symb])
            .await
            .unwrap();
        asset_data.load_all(symb, &p).await;
        let intv: &str = "1min";
        //let k:Kline=kfrom_sql(&p,&format!["kline_{}",&intv], None).await;
        //.nondt_test(&p,"nigger2",None).await;
        /*
        for intv in Intv::iter(){
            let s:&str=intv.to_str();
            println!("{:?}",s);
        }
         */
        assert!(true);
        println!("--------------------------------------");
    }
    #[tokio::test]
    async fn binnce_dl_test() {
        let test_struct = SQLConn::new_testing();
        test_struct.dl_binance_asset("TRUMPUSDT", None, false).await;
        assert!(true);
    }
}
