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
const metadata_db_path:&str="./databases/metadata.db";

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
        "INSERT OR REPLACE INTO {}( [Timestamp MS], [Open Time], Open, High, Low, Close, Volume, [Close Timestamp MS], [Close Time], [Quote Asset Volume], [Number of Trades], [Taker Buy Base Asset Volume], [Taker Buy Quote Asset Volume] ) ",
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
    let table_name = format!("kline_{}", intv.to_str());
    let input_iter=input.kline.chunks(*iter_size); //sqlite max variables  - 999
    for chunk in input_iter{
        append_kline(
            pool,
            &table_name,
            &chunk,
        )
        .await?;
    };
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
            Intv::Month1 =>1 * 60 * 60 * 24 * 30, //NOTE this is a hack... but it works for now
        }
    }
    pub fn to_ms(&self) -> i64 {
        &self.to_sec()*1000 as i64
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
            Intv::Month1 => 1 * 60 * 24 * 30,
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
    pub fn new_sql(
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
    pub dat: HashMap<Intv, Kline>,
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
    pub fn insert(&mut self, i: &Intv, kl: Kline) {
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

#[derive(Debug, Default, Clone)]
pub struct DLAsset{
    pub asset:String,
    pub exchange:String,
    pub dat_start_t:i64,
    pub dat_end_t:i64,
}

#[derive(Debug, Default)]
pub struct AssetData {
    pub id:usize,
    pub size: usize,
    pub kline_data: HashMap<String, Klines>,
    pub downloaded_assets:Vec<DLAsset>
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
            downloaded_assets:vec![]
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
    pub fn find_slice_n(
        &self,
        symbol: &str,
        intv: &Intv,
        start_time: &i64,
        next_wicks: u16,
    ) -> Option<&[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)]> {
        let kk: &Klines;
        let klines = self.kline_data.get(symbol);
        match klines {
            Some(k) => kk = k,
            None => { tracing::error!["Find slice error: out of bounds!"]; return None},
        }
        let kline = kk.dat.get(intv);
        let kl=match kline {
            Some(ki) => ki,
            None => {tracing::error!["Kline not found for interval!"]; return None},
        };
        let step: i64 = intv.to_ms();
        let end_time = step*(next_wicks as i64) + start_time;

        assert!(end_time > *start_time, "Start time must be greater than end time!");
        let dur= (end_time - start_time);


        let (start_index,end_index):(usize,usize) = if kl.kline.is_empty() ==false{
            //NOTE this is to avoid using let index = vec.iter().position(|&r| r == "n").unwrap();
            let kline_start_t=kl.kline[0].0.timestamp_millis();
            let kline_end_t=kl.kline[kl.kline.len()-1].0.timestamp_millis();
            let kline_length=kl.kline.len();

            let si=(start_time/(kline_end_t-kline_start_t)) as usize;
            let start_index:usize=si*kline_length;
            let confirm:i64=kl.kline[start_index].0.timestamp_millis();
            let start_index=if confirm as usize !=start_index{
                let ss=if start_index < confirm as usize{
                    let s=start_index+1;
                    s
                }else{
                    let s=start_index-1;
                    s
                };
                ss
            }else{
                start_index
            };
            let end_index=start_index+(next_wicks as usize);
            tracing::debug!["Start index:{}, End index:{}", start_index, end_index];
            (start_index, end_index)
        }else{
            tracing::error!["Kline empty!"];
            return None;
        };

        return Some(&kl.kline[start_index..end_index]);
    }
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
    start_time: Option<i64>,
    part_download: Option<i64>,
) -> Result<FatKline> {
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

        tracing::trace!["Kline conv in = {:?}",&input];
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
    let timestamp=match start_time{
        Some(start_time)=> start_time,
        None=>0,
    };
    tracing::debug!["INTV DL Timestamp: {}", timestamp];

    //+1 to avoid duplicates)
    let curr_timestamp=SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let no_datapoints = (curr_timestamp - timestamp)/intv.to_ms();
    //Ceil division is default s + 1 iterations if fin
    let no_iterations=no_datapoints/500;
    //500 is the hard limit for non API key downloads... i think
    //
    let mut kline:Vec<(
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
        )>=vec![];
    let mut progress_bar=Progress::new();
    let no_it= if no_iterations <=1{
        1
    }else{
        no_iterations
    };
    for n in (0..no_it) {
        let res=client.get_klines(symbol, intv.to_bin_str(), None, Some((timestamp+intv.to_ms()*n*500) as u64+1), Some((timestamp+intv.to_ms()*(n+1)*500) as u64));
        let kunt:Result<KlineSummaries>=match res{
            Ok(k)=>Ok(k),
            Err(err) => Err(anyhow!("Binance error:{:?}", err)),
        };
        let k=kunt?;
        tracing::debug!["Kline chunk: {:?}", k];
        tracing::debug!["Kline timestamp: {}", chrono::NaiveDateTime::from_timestamp_millis(timestamp+intv.to_ms()*n*500).ok_or(anyhow!["FFFUCK"]).expect("FFFUCUCUKJCK")];
        let klines=match k{
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
        let mut kline_chunk = kl?;
        kline.append(&mut kline_chunk);
        let current_timestamp_naive=chrono::NaiveDateTime::from_timestamp_millis(curr_timestamp).ok_or(anyhow!["current timestamp couldn't be formatted"])?;
        let timestamp_naive=chrono::NaiveDateTime::from_timestamp_millis(timestamp).ok_or(anyhow!["timestamp couldn't be formatted"])?;
        let bar: Bar = progress_bar
            .bar(no_it as usize, format!("Downloading data for {} {} between: {} and: {}: {}/{}",&symbol, &intv.to_str(), timestamp_naive, current_timestamp_naive, n, no_iterations));
        progress_bar.inc_and_draw(&bar, n as usize);
    };
    return Ok(FatKline::new(kline));
}
//TODO test and make functions to load an asset into asset data by downloading it
//
//

use linya::{Bar, Progress};
async fn create_metadata_db()->Result<()>{
    create_db(&metadata_db_path)
        .await
        .context(anyhow!("SQL::Unable cereate db"))?;
    let pool = SqlitePool::connect(&metadata_db_path)
        .await
        .context(anyhow!("SQL::Unable to metadata connect to db"))?;
    let q = format!(
        "CREATE TABLE assets ( [Asset] TEXT, [Exchange] TEXT, [Status] TEXT, [BaseAsset] TEXT, [QouteAsset] TEXT, [Extras] TEXT, [Extras Multi] TEXT, [Start Time ms] INTEGER, [End Time ms] INTEGER, [Market cap] REAL )"
    );
    exec_query(&pool, &q).await?;
    let q=format!(
        "CREATE UNIQUE INDEX Asset
        ON assets ( [Asset] )"
    );
    exec_query(&pool, &q).await?;

    let q = format!(
        "CREATE TABLE assets_fut ( [Asset] TEXT, [Exchange] TEXT, [Status] TEXT, [BaseAsset] TEXT, [QouteAsset] TEXT, [Extras] TEXT, [Extras Multi] TEXT, [onboardDate] INTEGER, [deliveryDate] INTEGER, [Market cap] REAL )"
    );
    exec_query(&pool, &q).await?;
    let q=format!(
        "CREATE UNIQUE INDEX AssetFut
        ON assets_fut ( [Asset] )"
    );
    exec_query(&pool, &q).await?;
    let q = format!(
        "CREATE TABLE assets_dl ( [Asset] TEXT, [Exchange] TEXT, [Start Time] INTEGER, [End Time] INTEGER )"
        //TODO make "default asset list for release version"
    );
    exec_query(&pool, &q).await?;
    let q=format!(
        "CREATE UNIQUE INDEX Asset_DL
        ON assets_dl ( [Asset] )"
    );
    exec_query(&pool, &q).await?;
    Ok(())
}

use crate::binance::{get_exchange_info, fut_get_exchange_info, SymbolInfo, FutSymbolInfo};

async fn download_asset_list_binance(metadata_db:&Pool<Sqlite>)->Result<()>{
    tracing::info!["Fetching exchange info"];
    let symbol_info_full=get_exchange_info().await?;
    tracing::info!["Fetching futures exchange info"];
    let fut_symbol_info_full=fut_get_exchange_info().await?;

    let symbol_inf=symbol_info_full.chunks(100);
    let fut_symbol_inf=fut_symbol_info_full.chunks(100);
    tracing::debug!["Symbol info: {:?}", symbol_inf];
    tracing::debug!["Symbol fut info: {:?}", fut_symbol_inf];

    for symbol_info in symbol_inf{
        let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
            "INSERT OR REPLACE INTO assets(  [Asset] , [Exchange] , [Status] , [BaseAsset] , [QouteAsset])"
        ));
        query_builder.push_values(
            symbol_info.iter(),
            |mut b, si|  {
                b.push_bind(si.symbol.clone())
                    .push_bind("Binance")
                    .push_bind(si.status.clone())
                    .push_bind(si.baseAsset.clone())
                    .push_bind(si.quoteAsset.clone());
            },
        );
        let mut query = query_builder.build();
        let result = query.execute(metadata_db).await?;
        log::info!("{:?}", result);
    };
    for fut_symbol_info in fut_symbol_inf{
        let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
            "INSERT OR REPLACE INTO assets_fut(  [Asset] , [Exchange] , [Status] , [BaseAsset] , [QouteAsset] , [onboardDate], [deliveryDate])"
        ));
        query_builder.push_values(
            fut_symbol_info.iter(),
            |mut b, si|  {
                b.push_bind(si.symbol.clone())
                    .push_bind("Binance")
                    .push_bind(si.status.clone())
                    .push_bind(si.baseAsset.clone())
                    .push_bind(si.quoteAsset.clone())
                    .push_bind(si.onboardDate.clone())
                    .push_bind(si.deliveryDate.clone());
            },
        );
        let mut query = query_builder.build();
        let result = query.execute(metadata_db).await?;
        log::info!("{:?}", result);
    };
    Ok(())
}
async fn dl_load_asset_list(pool:&Pool<Sqlite>)->Result<Vec<(String,String)>>{
    let out:Vec<(String, String)> = sqlx::query_as(format!("SELECT 
[Asset] , [Exchange] FROM assets_dl;"
            
            ).as_str())
        .fetch_all(pool)
        .await?;
    Ok(out)
}
async fn load_asset_list_ad(pool:&Pool<Sqlite>)->Result<Vec<DLAsset>>{
    let out:Vec<(String, String, i64, i64)> = sqlx::query_as(format!("SELECT 
[Asset] , [Exchange], [Start Time], [End Time] FROM assets_dl;"
            
            ).as_str())
        .fetch_all(pool)
        .await?;
    let oo:Vec<DLAsset>=out.iter().map(| (symbol, exchange, start_ms, end_ms) | {
        DLAsset{
            asset:symbol.to_string(),
            exchange:exchange.to_string(),
            dat_start_t:*start_ms,
            dat_end_t:*end_ms,
        }
    }).collect();
    Ok(oo)
}

async fn load_asset_list(pool:&Pool<Sqlite>)->Result<Vec<(String, String, String, String, String, Option<String>, Option<String>, Option<i64>, Option<i64>, Option<f64>)>>{
    let out:Vec<(String, String, String, String, String, Option<String>, Option<String>, Option<i64>, Option<i64>, Option<f64>)> = sqlx::query_as(format!("SELECT 
[Asset] , [Exchange] , [Status] , [BaseAsset] , [QouteAsset] , [Extras] , [Extras Multi] , [Start Time ms] , [End Time ms] , [Market cap] FROM assets;"
            
            ).as_str())
        .fetch_all(pool)
        .await?;
    Ok(out)
}

async fn update_asset_metadata(metadata_pool:&Pool<Sqlite>, symbol:&str,  start_time:Option<i64>,end_time:i64)->Result<()>{
    if let Some(start_time) = start_time{
        let q = format!(
            "
        INSERT OR REPLACE INTO metadata ( Asset, [Start Time ms], [End Time ms])
        VALUES('{}', {}. {});",
        symbol, start_time, end_time
        );
        exec_query(metadata_pool, &q).await?;
    }else{
        let q = format!(
            "
        INSERT OR REPLACE INTO assets_dl ( Asset, [End Time ms])
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
            "0" => Exchange::Binance,
            "1" => Exchange::Yahoo,
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

async fn download_asset_data(symbol:&str, exchange:&Exchange, start_time:Option<i64>, end_time:Option<i64>)->Result<Klines>{
    let klines= match exchange{
        Exchange::Binance=>{
            tracing::debug!["Downloading kline data for: {} from timestamp {:?}", symbol, start_time];
            intv_dl_klines(symbol, start_time, end_time).await
        }
        Exchange::Yahoo=>{
            todo!()
        }

    }?;
    Ok(klines)
}

const ITER_SIZE:usize=100;
//for 11 variables max is 999/11




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

async fn intv_dl_klines(
    symbol: &str,
    start_time:Option<i64>,
    end_time: Option<i64>,
) -> Result<Klines> {
    let s = symbol.to_string();
    let klines: Result<Klines, anyhow::Error> =
        tokio::task::spawn_blocking(move || {
            let client: Market = Binance::new(None, None);
            let mut klines = Klines::new_empty();
            let mut res: Result<_, anyhow::Error> = Err(anyhow![""]);
            let mut err_occured = false;
            for i in Intv::iter() {
                let kl = get_data_binance(&client, &s, i, start_time, end_time)?;
                if kl.kline.is_empty() {
                    tracing::info![
                        "Kline empty: {:?}, for interval {}",
                        &kl,
                        i.to_str()
                    ];
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


async fn cr_kl_tables(pool: &Pool<Sqlite>)->Result<()>{
    for i in Intv::iter() {
        let q = format!(
            "CREATE TABLE kline_{} ( [Timestamp MS] INTEGER, [Open Time] datetime,  Open REAL,  High REAL,  Low REAL,  Close REAL,  Volume REAL, [Close Timestamp MS] INTEGER, [Close Time] INTEGER,  [Quote Asset Volume] REAL,  [Number of Trades] INTEGER,  [Taker Buy Base Asset Volume] REAL,  [Taker Buy Quote Asset Volume] REAL,[Ignore], [Pattern hash] REAL )",
            i.to_str()
        );
        exec_query(&pool, &q).await?;
        let q=format!(
            "CREATE UNIQUE INDEX time_index_{}
            ON kline_{} ( [Timestamp MS] )",
            i.to_str(), i.to_str()
        );
        exec_query(&pool, &q).await?;
    };
    let q = format!(
        "CREATE TABLE colstats ( Column TEXT, Type INTEGER, Mean REAL, [Standard Deviaton] REAL , No INTEGER )",
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
    Ok(())
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
        "CREATE TABLE colstats ( Column TEXT, Type INTEGER, Mean REAL, [Standard Deviaton] REAL , No INTEGER )",
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
async fn update_asset_metadata_time(asset_pool:&Pool<Sqlite>, metadata_pool:&Pool<Sqlite>, symbol:&str, exchange:&str, st:i64, et:i64)->Result<()>{
    let ex=match exchange{
        "Binance" => Ok(0),
        "Yahoo" => Ok(1),
        _=>Err(anyhow!["Invalid exchange string"])
    }?;
    let q = format!(
        "
    INSERT OR REPLACE INTO metadata (Symbol, Exchange, [End Time], [Start Time] )
    VALUES('{}', '{}', {}, {});",
    symbol, ex, et ,st,
    );
    exec_query(&asset_pool, &q).await?;
    let q2 = format!(
        "
    INSERT OR REPLACE INTO assets_dl ( Asset, Exchange, [End Time], [Start Time] )
    VALUES('{}', '{}', {}, {});",
    symbol, exchange, et ,st,
    );
    exec_query(&metadata_pool, &q2).await?;
    Ok(())
}

async fn check_if_columns_exist(pool:&Pool<Sqlite>,columns:&str,table:&str)->Result<bool>{
    let start: (chrono::NaiveDateTime, f64, f64, f64, f64, f64) =
        sqlx::query_as(&format!["PRAGMA table_info({}) WHERE name = '{}'", columns,table])
            .fetch_one(pool)
            .await?;
    todo!();
}
//NOTE for text queries '' is needed... 

async fn get_asset_timestamps(symbol:&str, metadata_db: &Pool<Sqlite>)->Result<(Option<i64>,Option<i64>)>{
    let q=&format!["SELECT [Start time], [End time] FROM assets_dl WHERE Asset = '{}';", symbol];
    let timestamps_meta: (Option<i64>, Option<i64>) =
        sqlx::query_as(
            q
        )
        .fetch_one(metadata_db)
        .await?;
    let qf=&format!["SELECT [onboardDate], [deliveryDate] FROM assets_fut WHERE Asset = '{}';", symbol];
    let timestamps_fut: (Option<i64>, Option<i64>) =
        sqlx::query_as(
            qf
        )
        .fetch_one(metadata_db)
        .await?;
    match timestamps_meta.0{
        Some(_)=>return Ok((timestamps_meta.0,timestamps_meta.1)),
        None =>()
    };
    Ok((timestamps_fut.0,timestamps_fut.1))
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
    #[instrument(level="debug")]
    async fn load_all_data(&mut self, symbol: &str) -> Result<()> {
        let s: String = symbol.to_string();
        let pool =
            connect_sqlite(format!["{}/Asset{}.db", &self.db_path, &symbol])
                .await
                .context("SQL : unable to connect to db")?;
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
    pub async fn del_single_asset(&self, asset_symbol:&str)->Result<()>{
        let db_path=format!["./databases/Asset{}.db",asset_symbol];
        let file_path = std::path::Path::new(&db_path);
        std::fs::remove_file(file_path)?;
        Ok(())
    }
    pub async fn del_all_assets(&self, asset_symbol:&str)->Result<()>{
        todo!()
    }
    pub async fn dl_single_asset_bin_wrap(&self, asset_symbol:&str)->Result<()>{
        let exchange="Binance";
        let meta_pool = SqlitePool::connect(&metadata_db_path)
            .await
            .context(anyhow!("SQL::Unable to metadata connect to db"))?;
        let current_timestamp:i64=match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(n) => {
                let r=n.as_millis().try_into();
                match r{
                    Ok(n)=>Ok(n),
                    Err(e)=>Err(anyhow!["{:?}",e]),
                }
            }
            Err(e) => Err(anyhow!["{:?}",e]),
        }?;
        self.download_single_asset(asset_symbol, exchange, &meta_pool, current_timestamp).await?;
        Ok(())
    }
    pub async fn sql_append_metadata(&self, start_time:i64, end_time:i64)->Result<()>{
        todo!()
    }
    pub async fn download_single_asset(&self, asset_symbol:&str, exchange:&str, meta_pool:&Pool<Sqlite>, current_timestamp:i64)->Result<()>{
        let db_path=format!["./databases/Asset{}.db",asset_symbol];
        if Sqlite::database_exists(&db_path).await? == false {
            create_db(&db_path).await?;
            let apool = connect_sqlite(&db_path)
                .await?;
            cr_kl_tables(&apool).await?;
            tracing::debug!["Created database {} and created tables", &db_path];

            apool.close().await;
        };
        let (start_time_ms, end_time_ms) = get_asset_timestamps(&asset_symbol, &meta_pool).await?;

        /////////////////////////////////////////////////////////////////////////////////////////////////////
        ///
        //NOTE debug
        let start_t=if let Some(start_ti)=start_time_ms{
            start_ti
        }else{
            0
        };
        let end_t=if let Some(end_ti)=end_time_ms{
            end_ti
        }else{
            0
        };

        tracing::trace!["get start times for:{} \n Start: {} \n End: {}", asset_symbol, NaiveDateTime::from_timestamp_millis(start_t).ok_or(anyhow!["FFCUK"])?, NaiveDateTime::from_timestamp_millis(end_t).ok_or(anyhow!["FFCUK"])?];
        //let start_time_ms=Some(1769950725000 as i64); //1st Feb 2026 NOTE testing only remov
        /////////////////////////////////////////////////////////////////////////////////////////////////////
        ///
        let st:String=if let Some(start_time_ms)=start_time_ms{
            let t=chrono::NaiveDateTime::from_timestamp_millis(start_time_ms).ok_or(anyhow!["HAS TO BE SOME HERE!"]).expect("HAS TO BE SOME HERE");
            format!["{}",t]
        }else{
            "None".to_string()
        };
        let et:String=if let Some(end_time_ms)=end_time_ms{
            let t=chrono::NaiveDateTime::from_timestamp_millis(end_time_ms).ok_or(anyhow!["HAS TO BE SOME HERE!"]).expect("HAS TO BE SOME HERE");
            format!["{}",t]
        }else{
            "None".to_string()
        };
        tracing::trace!["Start end timestamps for: {} Start: {:?} End: {:?}",asset_symbol, st, et];
        let exch = Exchange::from(exchange);


        let klines=download_asset_data(&asset_symbol, &exch, end_time_ms, Some(current_timestamp)).await?;

        let apool = connect_sqlite(&db_path)
            .await?;
        for i in Intv::iter(){
            let kline=klines.full_dat.get(&i).ok_or(anyhow!["Kline interval not found!"])?;
            tracing::trace!["sql_append_kline_iter called on length of: {}, interval: {:?}", &kline.kline.len(), &i];
            if kline.kline.is_empty() == false{
                let res=sql_append_kline_iter(&kline, &i, &ITER_SIZE, &apool).await;
                match res{
                    Ok(_) => Ok(()),
                    Err(e) =>{
                        tracing::error!["Download asset binance SQL error: {}",e];
                        Err(e)
                    }
                }?;
            }else{
                tracing::info!["SQL append kline empty, doing nothing"]
            }
        }
        let start_timestamp=match start_time_ms{
            Some(start_timestamp) => start_timestamp,
            None => {
                tracing::error!["WRONG START TIME: NONE"];
                0
            },
        };


        let res=update_asset_metadata_time(&apool, &meta_pool, asset_symbol, "Binance", start_timestamp, current_timestamp ).await;
        match res{
            Ok(_)=>(),
            Err(e)=>{
                tracing::error!["Error dl_single_asset: {:?}",e]
            }
        };

        //TODO append extra functions here, when metadata received
        apool.close().await;
        Ok(())
    }
    pub async fn update_data(&self)->Result<()>{
        tracing::debug!["Update_data called"];
        //check if database exists, create if not
        if Sqlite::database_exists(&metadata_db_path).await? ==false {
            create_metadata_db().await?;
            tracing::debug!["Metadata DB created"];
        };
        let meta_pool = SqlitePool::connect(&metadata_db_path)
            .await
            .context(anyhow!("SQL::Unable to metadata connect to db"))?;
        download_asset_list_binance(&meta_pool).await?; 


        //NOTE download both asset lists
        //locally handle the above error 
        tracing::debug!["Asset list updated"];
        let asset_list=dl_load_asset_list(&meta_pool).await?;
        tracing::debug!["DL asset list loaded"];
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
        tracing::debug!["Current Timestamp {}",current_timestamp_ms];


        //NOTE the current system uses asset futures to get the onboarding date... why the v3 API
        //doesn't have it... idk 
        //
        //Asset list loaded here, and operations executed for each asset
        //
        meta_pool.close().await;

        let meta_pool = SqlitePool::connect(&metadata_db_path)
            .await
            .context(anyhow!("SQL::Unable to metadata connect to db"))?;

        for ( asset_symbol, exchange) in asset_list {
            self.download_single_asset(&asset_symbol, &exchange, &meta_pool, current_timestamp_ms).await;
        };
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
    async fn load_asset_list(&mut self) -> Result<()> {
        let ad_a = Arc::clone(&mut self.hist_asset_data);
        let meta_pool = SqlitePool::connect(&metadata_db_path)
            .await
            .context(anyhow!("SQL::Unable to metadata connect to db"))?;
        let dl_list=load_asset_list_ad(&meta_pool).await?;
        let mut ad = ad_a.lock().expect("Posioned AD mutex! (DATA)");
        let result = ad.downloaded_assets=dl_list;
        Ok(())
    }
    async fn insert_dl_asset(&self, symbol:&str, exchange:&str)->Result<()>{
        let meta_pool = SqlitePool::connect(&metadata_db_path)
            .await
            .context(anyhow!("SQL::Unable to metadata connect to db"))?;


        let validate=self.validate_asset_binance(&meta_pool,symbol,exchange).await?;
        match validate{
            true=>(),
            false=>
            {
                tracing::error!["Asset symbol:{} not found on Binance!",symbol];
                meta_pool.close().await;
                return Ok(())
            }
        };
        let q = format!(
            "
        INSERT OR REPLACE INTO metadata (Symbol, Exchange, [End Time], [Start Time] )
        VALUES('{}', '{}');",
        symbol, exchange
        );
        exec_query(&meta_pool, &q).await?;
        meta_pool.close().await;
        Ok(())
    }
    async fn validate_asset_binance(&self, meta_pool:&Pool<Sqlite>, symbol:&str, exchange:&str)->Result<bool>{
        let res:(Option<String>,)= sqlx::query_as(format!("SELECT 
    [Asset] FROM assets WHERE Asset = {};",
                symbol
                
                ).as_str())
            .fetch_one(meta_pool)
            .await?;
        match res.0{
            Some(_)=>Ok(true),
            None=>Ok(false)
        }
    //NOTE this return a bug that will need to be fixed later, when addint
    //YAHOO. bit is fine for now. Maybe make a different asset list for
    //each exhcange...or simply append yahoo
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
            SQLInstructs::DelAsset{symbol:ref symbol} => {
                todo!()
            }
            SQLInstructs::DelAll=> {
                todo!()
            }
            SQLInstructs::UpdateDataBinance {symbol:ref symbol} => {
                let res=self.dl_single_asset_bin_wrap(&symbol).await;
                let resp=match res {
                    Ok(_) => {
                         SQLResponse::Success
                    },
                    Err(e) => {
                        let err_string=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} SQL::update_all_data:{:?}", i, e.context(err_ctx)]
                        );
                        SQLResponse::Failure((err_string, GeneralError::Generic))
                    }
                };
                resp
            }
            SQLInstructs::UpdateDataAll=> {
                let res=self.update_data().await;
                let resp=match res {
                    Ok(_) => {
                         SQLResponse::Success
                    },
                    Err(e) => {
                        let err_string=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} SQL::update_all_data:{:?}", i, e.context(err_ctx)]
                        );
                        SQLResponse::Failure((err_string, GeneralError::Generic))
                    }
                };
                resp
            }
            SQLInstructs::LoadDLAssetList=> {
                let res=self.load_asset_list().await;
                let resp=match res {
                    Ok(_) => {
                         SQLResponse::Success
                    },
                    Err(e) => {
                        let err_string=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} SQL::load_asset_list:{:?}", i, e.context(err_ctx)]
                        );
                        SQLResponse::Failure((err_string, GeneralError::Generic))
                    }
                };
                resp
            }
            SQLInstructs::InsertDLAsset{ref symbol,ref exchange}=> {
                let res=self.insert_dl_asset(&symbol, &exchange).await;
                let resp=match res {
                    Ok(_) => {
                         SQLResponse::Success
                    },
                    Err(e) => {
                        let err_string=format!["{}",e];
                        log::error!(
                            "{}",
                            anyhow!["{:?} SQL::load_asset_list:{:?}", i, e.context(err_ctx)]
                        );
                        SQLResponse::Failure((err_string, GeneralError::Generic))
                    }
                };
                resp
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
