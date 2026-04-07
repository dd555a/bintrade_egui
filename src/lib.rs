use crate::gui::Settings;
use crate::trade::Order;
use std::collections::HashMap;
use strum_macros::EnumIter;
use bincode::{Decode, Encode};

#[derive(Eq, PartialEq, Debug, Clone, Copy, Hash, Encode,Decode)]
pub enum GeneralError {
    SystemError,
    GetError,
    PutError,
    DATAError,
    Generic,
}

#[derive(Eq, PartialEq, Debug, Clone, Hash, Encode,Decode, Default)]
pub enum ProcResp {
    BinResp(BinResponse),
    SQLResp(SQLResponse),
    #[default]
    Client,
}

#[derive(PartialEq,Default, Debug, Clone, Encode,Decode)]
pub enum SQLInstructs {
    #[default]
    None,
    UpdateSettings(Settings),
    LoadHistData {
        symbol: String,
    },
    LoadHistDataPart {
        symbol: String,
        start: i64,
        end: i64,
    },
    LoadHistDataPart2 {
        symbol: String,
        backload_wicks: i64,
        trade_time: i64,
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
    ValidateDLAsset {
        symbol: String,
    },
    ValidateBinanceAsset {
        symbol: String,
    },
}
impl SQLInstructs {
    pub fn to_str(&self) -> &str {
        match &self {
            SQLInstructs::None => "SQLInstructs: None",
            SQLInstructs::UpdateSettings(_) => "SQLInstructs: update settings",
            SQLInstructs::LoadHistData { symbol: _ } => "SQLInstructs: Load Hist Data",
            SQLInstructs::LoadHistDataPart {
                symbol: _,
                start: _,
                end: _,
            } => "SQLInstructs: Load Hist Data partially",
            SQLInstructs::LoadHistDataPart2 {
                symbol: _,
                trade_time: _,
                backload_wicks: _,
            } => "SQLInstructs: Load Hist Data partially2",
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
            SQLInstructs::ValidateDLAsset { symbol: _ } => {
                "SQLInstructs: Validate symbol for download"
            }
            SQLInstructs::ValidateBinanceAsset { symbol: _ } => {
                "SQLInstructs: Validate symbol for download"
            }
        }
    }
}

#[derive(Eq, PartialEq, Debug, Clone, Hash, Encode,Decode)]
pub enum SQLResponse {
    None,
    Success,
    Failure((String, GeneralError)),
}

#[derive(Eq, PartialEq, Debug, Clone, Hash, Encode,Decode)]
pub enum BinResponse {
    None,
    Success,
    Failure((String, GeneralError)),
    OrderStatus(u64),
}

#[derive(PartialEq, Default, EnumIter, Debug, Clone, Encode,Decode)]
pub enum BinInstructs {
    #[default]
    None,
    ConnectWS {
        params: HashMap<String, Vec<String>>,
    },
    ReConnectWS,
    ConnectUserWS {
        params: Vec<String>,
    },
    Disconnect,
    GetUserData,
    UpdateSettings(Settings),
    GetAllBalances,
    GetBalance {
        symbol: String,
    },
    AddReplaceApiKeys {
        pub_key: String,
        priv_key: String,
    },
    RemoveApiKeys,
    PlaceOrder {
        symbol: String,
        o: Order,
    },
    CancelAndReplaceOrder {
        id: u64,
        symbol: String,
        o: Order,
    },
    CancelOrder {
        id: u64,
        symbol: String,
        o: Order,
    },
    CancelAllOrders {
        symbol: String,
    },
    ChangeLiveAsset {
        symbol: String,
        defualt_symbol: String,
    },
    ChangeLiveAsset2 {
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
            BinInstructs::ReConnectWS => "BinInstruct: Reconnect",
            BinInstructs::UpdateSettings(_) => "BinInstructs: update settings",
            BinInstructs::GetUserData => "BinInstruct: GetUserData",
            BinInstructs::GetAllBalances => "Get all balances Binance",
            BinInstructs::GetBalance { symbol: _ } => "Get balance for a Symbol",
            BinInstructs::RemoveApiKeys => "Remove API keys",
            BinInstructs::AddReplaceApiKeys {
                pub_key: _,
                priv_key: _,
            } => "Add or replace API keys",
            BinInstructs::PlaceOrder { symbol: _, o: _ } => "BinInstruct: Place Order",
            BinInstructs::CancelAndReplaceOrder {
                id: _,
                symbol: _,
                o: _,
            } => "Cancel and replace order",
            BinInstructs::CancelOrder {
                id: _,
                symbol: _,
                o: _,
            } => "Cancel and Orderreplace order",
            BinInstructs::CancelAllOrders { symbol: _ } => "BinInstruct: Cancel All Orders",
            BinInstructs::ChangeLiveAsset {
                symbol: _,
                defualt_symbol: _,
            } => "BinInstruct: Change symbol",
            BinInstructs::ChangeLiveAsset2 { symbol: _ } => "BinInstruct: Change symbol2",
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

#[derive(Debug, Clone)]
pub enum ClientInstruct {
    None,

    Stop,
    Start,
    Terminate,
    RestartRemote,
    UpdateSettings(Settings),

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
            ClientInstruct::UpdateSettings(_) => "Update Settings",
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
