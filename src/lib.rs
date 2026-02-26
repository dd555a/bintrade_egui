#![allow(warnings)]
use crate::trade::Order;
use serde_json::Value;
use strum_macros::EnumIter;

#[derive(Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub enum Sys_err {
    No_Network,
    DiskFull,
}
#[derive(Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub enum Get_err {
    Download_failed,
    Not_found,
    Access_denied,
}
#[derive(Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub enum Put_err {
    Upload_failed,
    Path_not_found,
}

#[derive(Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub enum Indp_data_err {
    Duplicates,
    Timezone,
    WrongFormat,
}
#[derive(Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub enum Dep_data_err {
    WrongFormat,
    Duplicates,
    Timezone,
    Incorrect_method,
}

#[derive(Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub enum DataError {
    Indp_corrup(Indp_data_err), //=> delete db, download again, etc...
    Depn_corrup(Dep_data_err),  //=> delete afflicted dependent tables and recalc...
}

#[derive(Eq, PartialEq, Debug, Clone, Copy, Hash)]
pub enum GeneralError {
    SystemError(Sys_err),
    GetError(Get_err),
    PutError(Put_err),
    DATAError(DataError),
    Generic,
}

#[derive(Eq, PartialEq, Debug, Clone, Hash, Default)]
pub enum ProcResp {
    BinResp(BinResponse),
    SQLResp(SQLResponse),
    #[default]
    Client,
}

#[derive(Eq, PartialEq, EnumIter, Debug, Clone, Default, Hash)]
pub enum SQLInstructs {
    #[default]
    None,
    LoadHistData {
        symbol: String,
    },
    LoadHistDataPart {
        symbol: String,
        start: i64,
        end: i64,
    },
    UnloadHistData {
        symbol: String,
    },
    LoadTradeRecord {
        id: u32,
    },
    UpdateDataBinance {
        symbol: String,
    },
    UpdateDataAll,
    DelAsset {
        symbol: String,
    },
    DelAll,
    LoadDLAssetList,
    InsertDLAsset {
        symbol: String,
        exchange: String,
    },
}
impl SQLInstructs {
    pub fn to_str(&self) -> &str {
        match &self {
            SQLInstructs::None => "SQLInstructs: None",
            SQLInstructs::LoadHistData { symbol: _ } => "SQLInstructs: Load Hist Data",
            SQLInstructs::LoadHistDataPart {
                symbol: _,
                start: _,
                end: _,
            } => "SQLInstructs: Load Hist Data partially",
            SQLInstructs::UnloadHistData { symbol: _ } => "SQLInstructs: Unload Hist Data",
            SQLInstructs::LoadTradeRecord { id: _ } => "SQLInstructs: Load Trade Record",
            SQLInstructs::UpdateDataBinance { symbol: _ } => "SQLInstructs: Update data binance",
            SQLInstructs::UpdateDataAll => "SQLInstructs: Update all data",
            SQLInstructs::DelAsset { symbol: _ } => "SQLInstructs: Delete data for an asset",
            SQLInstructs::DelAll => "SQLInstructs: Delete all data",
            SQLInstructs::LoadDLAssetList => "Load downloaded asset list",
            SQLInstructs::InsertDLAsset {
                symbol: _,
                exchange: _,
            } => "SQLInstructs: Insert symbol for download",
        }
    }
}

#[derive(Eq, PartialEq, Debug, Clone, Hash)]
pub enum SQLResponse {
    None,
    Success,
    Failure((String, GeneralError)),
}

#[derive(Eq, PartialEq, Debug, Clone, Hash)]
pub enum BinResponse {
    None,
    Success,
    Failure((String, GeneralError)),
    OrderStatus(i32),
}

use crate::conn::SymbolOutput;
use std::collections::HashMap;

#[derive(PartialEq, EnumIter, Debug, Clone, Default)]
pub enum BinInstructs {
    #[default]
    None,
    ConnectWS {
        params: HashMap<String, Vec<String>>,
    },
    ConnectUserWS {
        params: Value,
    },
    Disconnect,
    GetUserData,
    PlaceOrder {
        symbol: String,
        o: Order,
    },
    CancelAndReplaceOrder {
        symbol: String,
        o: Order,
    },
    CancelAllOrders {
        symbol: String,
    },
}
impl BinInstructs {
    pub fn to_str(&self) -> &str {
        match &self {
            BinInstructs::None => "BinInstruct: None",
            BinInstructs::ConnectWS { params: _ } => "BinInstruct: Connect WS",
            BinInstructs::ConnectUserWS { params: _ } => "BinInstruct: Connect User WS",
            BinInstructs::Disconnect => "BinInstruct: Disconnect",
            BinInstructs::GetUserData => "BinInstruct: GetUserData",
            BinInstructs::PlaceOrder { symbol: _, o: _ } => "BinInstruct: Place Order",
            BinInstructs::CancelAndReplaceOrder { symbol: _, o: _ } => {
                "BinInstruct: Cancel and Replace Order:"
            }
            BinInstructs::CancelAllOrders { symbol: _ } => "BinInstruct: Cancel All Orders",
        }
    }
}

#[derive(Eq, PartialEq, EnumIter, Debug, Clone)]
pub enum ClientResponse {
    None,
    Success,
    Failure((String, u32)),
    ProcResp(ProcResp),
}

#[derive(PartialEq, EnumIter, Debug, Clone)]
pub enum ClientInstruct {
    None,

    Stop,
    Start,
    Terminate,
    RestartRemote,

    //Binance client instructions
    StartBinCli,
    StopBinCli,
    SendBinInstructs(BinInstructs),

    //SQL instructions(SQLITE start calculations Nshit)
    StartSQL,
    StopSQL,
    SendSQLInstructs(SQLInstructs),

    //Ping pong the server
    Ping(i64),
    Pong(i64),
}
impl ClientInstruct {
    pub fn to_str(&self) -> &str {
        match &self {
            ClientInstruct::None => "None",
            ClientInstruct::Stop => "Stop",
            ClientInstruct::Start => "Start",
            ClientInstruct::Terminate => "Terminate",

            ClientInstruct::RestartRemote => "Restart Remote",
            ClientInstruct::StartBinCli => "Start Bin Client",
            ClientInstruct::StopBinCli => "Stop Bin Client",
            ClientInstruct::SendBinInstructs(_) => "Send Bin Instructs",

            ClientInstruct::StartSQL => "Start SQL",
            ClientInstruct::StopSQL => "Stop SQL",
            ClientInstruct::SendSQLInstructs(_) => "Send SQL Instructs",

            ClientInstruct::Ping(_) => "Ping",
            ClientInstruct::Pong(_) => "Pong",
        }
    }
}
impl std::fmt::Display for ClientInstruct {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.to_str())
    }
}
pub mod client;
pub mod conn;
pub mod data;
pub mod gui;
pub mod trade;
