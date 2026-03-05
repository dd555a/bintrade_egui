use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{Read, Write};
use std::num::ParseIntError;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use strum::IntoEnumIterator;
use strum_macros::EnumIter;

use anyhow::{Result, anyhow};
use tracing::instrument;

use tokio::sync::watch;

use eframe::egui;
use egui::{Color32, ComboBox, RichText, epaint};
use egui_extras::{Column, TableBuilder};
use egui_plot_bintrade::{
    AxisHints, Bar, BarChart, BoxElem, BoxPlot, BoxSpread, GridInput, GridMark, HLine, HPlacement,
    Legend, LineStyle as LineStyleEgui, Plot,
};
use egui_tiles::{Tile, TileId, Tiles};
use epaint::Stroke;

use bincode::{Decode, Encode, config};
use chrono::Datelike;
use derive_debug::Dbg;
use magic_crypt::{MagicCryptTrait, new_magic_crypt};

use crate::conn::{KlineTick, SymbolOutput};
use crate::data::{AssetData, DLAsset, Intv, Klines};
use crate::trade::{HistTrade, LimitStatus, Order, Quant, StopStatus};
use crate::{BinInstructs, ClientInstruct, ClientResponse, ProcResp, SQLInstructs, SQLResponse};

const WICKS_VISIBLE: usize = 90;
const NAVI_WICKS_DEFAULT: u16 = 30;
const CHART_FORWARD: u16 = 40;
const DEFAULT_TRADE_WICKS: u16 = 30;
const BACKLOAD_WICKS: i64 = 720;

const SETTINGS_SAVE_PATH: &str = "./Settings.bin";

#[derive(Dbg, Clone)]
struct KlinePlot {
    l_boxplot: Vec<BoxElem>,
    l_barchart: Vec<Bar>,

    l_tick_boxplot: Vec<BoxElem>,
    l_tick_barchart: Vec<Bar>,

    tick_kline: Option<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>,

    intv: Intv,
    name: String,
    loading: bool,
    static_loaded: bool,
    chart_params: (f64, f64),
    x_bounds: (f64, f64),
    y_bounds: (f64, f64),
    v_bound: f64,

    tick_highest: f64,
    tick_lowest: f64,

    symbol: String,

    hlines: Vec<HLine>,
    navi_wicks_s: String,
    navi_wicks: usize,

    get_data_timestamp: Option<i64>,

    ticks: usize,
    offset: i64,
    y_offset: i64,
    y_offset_s: String,
    y_increment: f64,
    x_bounds_set: bool,

    live_asset_changed: bool,
}
impl Default for KlinePlot {
    fn default() -> Self {
        Self {
            l_boxplot: vec![],
            l_barchart: vec![],
            l_tick_boxplot: vec![],
            l_tick_barchart: vec![],
            tick_kline: None,
            intv: Intv::Min1,
            name: "".to_string(),
            loading: false,
            static_loaded: false,
            chart_params: (1.0, 1.0),
            x_bounds: (0.0, 100.0),
            y_bounds: (0.0, 100.0),
            v_bound: 100.0,
            symbol: "BTCUSDT".to_string(),

            tick_highest: 0.0,
            tick_lowest: 0.0,

            get_data_timestamp: None,

            hlines: vec![],

            navi_wicks_s: "30".to_string(),
            y_offset_s: "10".to_string(),

            navi_wicks: 30,
            ticks: 0,
            offset: 0,
            y_offset: 0,
            y_increment: 0.001,
            x_bounds_set: false,

            live_asset_changed: false,
        }
    }
}

impl KlinePlot {
    fn show_empty(&self, ui: &mut egui::Ui) {
        let (plot_candles, plot_volume) = self.mk_plt();
        let bp = BoxPlot::new(&self.name, vec![]);
        plot_candles.show(ui, |plot_ui| {
            plot_ui.box_plot(bp);
        });
        let bc = BarChart::new(&self.name, vec![]);
        plot_volume.show(ui, |plot_ui| {
            plot_ui.bar_chart(bc);
        });
    }
    fn show_live(
        &mut self,
        ui: &mut egui::Ui,
        plot_extras: &PlotExtras,
        live_ad: Arc<Mutex<AssetData>>,
        collected_data: Option<&HashMap<String, SymbolOutput>>,
        live_info: Option<&mut LiveInfo>,
        return_wicks: Option<usize>,
        last_price_hist: Option<&mut f64>,
        hist_symbol_info: Option<&mut (String, String, String)>,
    ) -> Option<Vec<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>> {
        let ad = live_ad.lock().expect("Live AD mutex locked");
        let ret = match return_wicks {
            Some(ret_wicks) => {
                let ret = ad.kline_data.get(&self.symbol);
                match ret {
                    Some(klines) => {
                        let ret2 = klines.dat.get(&self.intv);
                        match ret2 {
                            Some(kline) => Some(kline.kline[ret_wicks..].to_vec()),
                            None => {
                                tracing::error![
                                    "Unable to find interval: {} in ad for ret_wicks",
                                    &self.intv.to_str()
                                ];
                                None
                            }
                        }
                    }
                    None => {
                        tracing::error![
                            "Unable to find symbol: {} in ad for ret_wicks",
                            &self.symbol
                        ];
                        None
                    }
                }
            }
            None => None,
        };

        //NOTE AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
        if let (Some(last_price_h), Some(hist_symbol_inf)) = (last_price_hist, hist_symbol_info) {
            if let Some(klines) = &ad.kline_data.get(&self.symbol) {
                hist_symbol_inf.0 = klines.asset_pair.clone();
                hist_symbol_inf.1 = klines.s1_string.clone();
                hist_symbol_inf.2 = klines.s2_string.clone();
                if let Some(kl) = klines.dat.get(&self.intv) {
                    if kl.kline.is_empty() == false {
                        *last_price_h = kl.kline[kl.kline.len() - 1].4;
                    };
                };
            };
        };

        let symbol = self.symbol.clone();
        if let Some(col_data) = collected_data {
            if let Some(data) = col_data.get(&symbol) {
                tracing::trace!["Collected data non empty"];
                let _res = self.live_live_from_ad(
                    &ad,
                    &symbol,
                    self.intv.clone(),
                    self.intv.to_view_window(),
                    None,
                    &data,
                );
            };
            let _res = self.live_from_ad(
                &ad,
                &symbol,
                self.intv.clone(),
                self.intv.to_view_window(),
                None,
                false,
            );
        } else {
            let _res = self.live_from_ad(
                &ad,
                &symbol,
                self.intv.clone(),
                self.intv.to_view_window(),
                None,
                true,
            );
        };
        let _res = self.show(ui, plot_extras);

        ui.label(RichText::new(format!["Current asset: {}", &symbol]).color(Color32::WHITE));
        egui::Grid::new("Kline navi:").show(ui, |ui| {
            if ui.button("<< Navi").clicked() {
                let res: Result<u16, ParseIntError> = self.navi_wicks_s.parse();
                let n_wicks = match res {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!["Parsing error for navigation wicks: {}", e];
                        NAVI_WICKS_DEFAULT
                    }
                };
                self.offset -= n_wicks as i64;
            };
            ui.add(egui::TextEdit::singleline(&mut self.navi_wicks_s).hint_text("Navi N wicks"));
            if ui.button("Navi >>").clicked() {
                let res: Result<u16, ParseIntError> = self.navi_wicks_s.parse();
                let n_wicks = match res {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!["Parsing error for navigation wicks: {}", e];
                        NAVI_WICKS_DEFAULT
                    }
                };
                self.offset += n_wicks as i64;
            }
            if ui.button("Reset offset x").clicked() {
                self.offset = 0;
            }
            if ui.button("Y+").clicked() {
                let res: Result<u16, ParseIntError> = self.y_offset_s.parse();
                let n_wicks = match res {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!["Parsing error for navigation wicks: {}", e];
                        NAVI_WICKS_DEFAULT
                    }
                };
                self.y_offset += n_wicks as i64;
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.y_offset_s).hint_text("Y IncrementN wicks"),
            );
            if ui.button("Y-").clicked() {
                let res: Result<u16, ParseIntError> = self.y_offset_s.parse();
                let n_wicks = match res {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!["Parsing error for navigation wicks: {}", e];
                        NAVI_WICKS_DEFAULT
                    }
                };
                self.y_offset -= n_wicks as i64;
            }
            if ui.button("Reset offset y").clicked() {
                self.y_offset = 0;
            }
        });
        return ret;
    }
    fn mk_plt(&self) -> (Plot<'_>, Plot<'_>) {
        let (x_lower, x_higher) = self.x_bounds;
        let (y_lower, y_higher) = self.y_bounds;
        let v_higher = self.v_bound;
        make_plot(
            &self.name, self.intv, y_lower, y_higher, x_lower, x_higher, v_higher,
        )
    }
    fn add_live(
        &mut self,
        kline_input: &[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)],
        divider: &f64,
        width: &f64,
        tick: bool,
    ) {
        if tick == false {
            self.l_boxplot = vec![];
            self.l_barchart = vec![];
        } else {
            self.l_tick_boxplot = vec![];
            self.l_tick_barchart = vec![];
        };
        let mut highest: f64 = 0.0;
        let mut v_highest: f64 = 0.0;
        let mut lowest: f64 = kline_input[0].3;
        let t0 = if kline_input.len() > WICKS_VISIBLE + 1 {
            kline_input[kline_input.len() - 1 - WICKS_VISIBLE].0 //NOTE out of bounds error
        } else {
            kline_input[0].0
        };
        let t1 = kline_input[kline_input.len() - 1].0;
        if tick == false {
            self.get_data_timestamp = Some(t1.timestamp());
        };
        if self.x_bounds_set == false {
            let w = (self.intv.to_sec() as f64) * (CHART_FORWARD as f64);
            let u = (self.intv.to_sec() as f64) * ((self.ticks as f64) + (self.offset as f64));
            tracing::trace!["ticks count:{}", self.ticks];
            self.x_bounds = (
                ((t0.timestamp() as f64) + u) / self.chart_params.0,
                ((t1.timestamp() as f64) + w + u) / self.chart_params.0,
            );
            //self.x_bounds_set = true;
        };
        for kline in kline_input.iter() {
            let (t, _, h, l, _, v) = kline;
            if h > &highest {
                highest = *h;
            };
            if v > &v_highest {
                tracing::trace!["V highest: {:?} \n", v];
                v_highest = *v;
            };
            if l < &lowest {
                lowest = *l;
            };
            tracing::trace!["V: {:?} \n", v];
            let (boxe, bar) = box_element(kline, divider, width);

            if tick == false {
                self.l_boxplot.push(boxe);
                self.l_barchart.push(bar);
            } else {
                if let Some(init_timestamp) = self.get_data_timestamp {
                    if t.timestamp() == init_timestamp {
                    } else {
                        self.l_tick_boxplot.push(boxe);
                        self.l_tick_barchart.push(bar);
                    };
                } else {
                    self.l_tick_boxplot.push(boxe);
                    self.l_tick_barchart.push(bar);
                };
                if h > &self.tick_highest {
                    self.tick_highest = *h;
                };
                if l < &self.tick_lowest {
                    self.tick_lowest = *l;
                };
            };
        }

        if tick == true {
            self.ticks = kline_input.len();
        };
        self.v_bound = v_highest;
        let (v_y, u_y) = (
            (1.0 + self.y_increment * self.y_offset as f64),
            (1.0 + self.y_increment * self.y_offset as f64),
        );
        if (self.tick_highest >= highest || self.tick_lowest <= lowest) && tick == true {
            self.y_bounds = (self.tick_lowest * v_y, self.tick_highest * u_y);
        } else {
            self.y_bounds = (lowest * v_y, highest * u_y);
        };
    }
    #[allow(unused)]
    fn show(&mut self, ui: &mut egui::Ui, plot_extras: &PlotExtras) -> Result<()> {
        let (plot_candles, plot_volume) = self.mk_plt();
        let bp = BoxPlot::new(&self.name, self.l_boxplot.clone())
            .element_formatter(Box::new(time_format));
        let bc = BarChart::new(&self.name, self.l_barchart.clone());

        let bp_tick2 = BoxPlot::new(&self.name, self.l_tick_boxplot.clone())
            .element_formatter(Box::new(time_format));
        let bc_tick2 = BarChart::new(&self.name, self.l_tick_barchart.clone());

        let hlines = self.hlines.clone();
        if let Some(tick_kline) = self.tick_kline {
            let (tick_box_e, tick_vol) =
                box_element(&tick_kline, &self.chart_params.0, &self.chart_params.1);
            let tick_box_plot = vec![tick_box_e];
            let tick_bar_vol = vec![tick_vol];
            let bp_tick = BoxPlot::new("Tick", tick_box_plot);
            tracing::trace!["Tick bar vol:{:?}", tick_bar_vol];
            let bc_tick = BarChart::new("Tick Vol", tick_bar_vol);
            if self.loading == false && self.static_loaded == true {
                plot_candles.show(ui, |plot_ui| {
                    plot_ui.box_plot(bp);
                    //plot_ui.bar_chart(bc);
                    plot_ui.box_plot(bp_tick);
                    plot_ui.box_plot(bp_tick2);
                    //plot_ui.bar_chart(bc_tick);
                });
                plot_volume.show(ui, |plot_ui| {
                    //plot_ui.box_plot(bp);
                    plot_ui.bar_chart(bc);
                    //plot_ui.box_plot(bp_tick);
                    plot_ui.bar_chart(bc_tick);
                    plot_ui.bar_chart(bc_tick2);
                });
            } else {
                self.show_empty(ui);
            }
        } else {
            if self.loading == false && self.static_loaded == true {
                plot_candles.show(ui, |plot_ui| {
                    tracing::trace!["Hlines in plot: {:?}", &hlines];
                    if hlines != [] {
                        for h in hlines {
                            tracing::trace!["Hlines (for h in ){:?}", &h];
                            plot_ui.add(h);
                        }
                    };
                    plot_ui.box_plot(bp);
                    plot_ui.box_plot(bp_tick2);
                    //plot_ui.bar_chart(bc);
                });
                plot_volume.show(ui, |plot_ui| {
                    plot_ui.bar_chart(bc);
                    plot_ui.bar_chart(bc_tick2);
                });
            } else {
                self.show_empty(ui);
            }
        }
        Ok(())
    }
    #[allow(unused)]
    fn live_live_from_ad(
        &mut self,
        ad: &AssetData,
        symbol: &str,
        intv: Intv,
        max_load_points: usize,
        timestamps: Option<(chrono::NaiveDateTime, chrono::NaiveDateTime)>,
        data: &SymbolOutput,
    ) -> Result<()> {
        if ad.live_asset_symbol_changed.0 == true {
            self.symbol = ad.live_asset_symbol_changed.1.clone();
            self.live_asset_changed = true;
        };

        let mut k = if let Some(ck) = data.closed_klines.get(&intv) {
            KlineTick::to_kline_vec(ck)
        } else {
            vec![]
        };
        let ok = if let Some(ok) = data.all_klines.get(&intv) {
            KlineTick::to_kline_vec(ok)
        } else {
            vec![]
        };
        if ok.is_empty() == false {
            let last_tick = ok[ok.len() - 1];
            k.push(last_tick);
        };
        let (div, width) = get_chart_params(&intv);
        self.chart_params = (div, width);
        if k.len() <= max_load_points {
            if k.is_empty() == false {
                self.add_live(&k, &div, &width, true);
            };
        } else {
            let kl = &k[(k.len() - max_load_points)..];
            if kl.is_empty() == false {
                self.add_live(&k, &div, &width, true);
            };
        }
        self.static_loaded = true;
        return Ok(());
    }
    //#[instrument(level="debug")]
    fn live_from_ad(
        &mut self,
        ad: &AssetData,
        symbol: &str,
        intv: Intv,
        max_load_points: usize,
        timestamps: Option<(chrono::NaiveDateTime, chrono::NaiveDateTime)>,
        hist: bool,
    ) -> Result<()> {
        tracing::trace!["GUI Live from AD called!"];
        let k = match timestamps {
            Some((start, end)) => ad.find_slice(symbol, &intv, &start, &end).ok_or(anyhow![
                "Unable to find slice in the period:{} to {}",
                &start,
                &end
            ])?,
            None => ad.load_full_intv(symbol, &intv)?,
        };
        tracing::trace!["Kline intv (live_from_ad) {}", intv.to_str()];
        let (div, width) = get_chart_params(&intv);
        self.chart_params = (div, width);
        if k.len() <= max_load_points {
            self.loading = true;

            if hist == true {
                tracing::trace!["HIST k.len <<< {:?}", k.is_empty()];
            };

            self.add_live(k, &div, &width, false);
            self.loading = false;
        } else {
            //tracing::debug!["max load points separation{:?}",k];
            let kl = &k[(k.len() - max_load_points)..];
            //tracing::debug!["max load points separation{:?}",kl];
            self.loading = true;

            if hist == true {
                tracing::trace!["HIST k.len >>>  {:?}", k.is_empty()];
            };

            self.add_live(kl, &div, &width, false);
            self.loading = false;
        }
        self.static_loaded = true;
        return Ok(());
    }
}

fn box_element(
    slice: &(chrono::NaiveDateTime, f64, f64, f64, f64, f64),
    divider: &f64,
    width: &f64,
) -> (BoxElem, Bar) {
    let (time, open, high, low, close, volume) = slice.clone();
    let a1 = (high + low) / 2.0;
    let red = Color32::from_rgb(255, 0, 0);
    let green = Color32::from_rgb(0, 255, 0);
    let width = width.clone();
    if open >= close {
        let bb: BoxElem = BoxElem::new(
            (time.timestamp() as f64) / divider,
            BoxSpread::new(low, close, a1, open, high),
        )
        .whisker_width(0.0)
        .fill(red)
        .stroke(Stroke::new(2.0, red))
        .name(format!["{}", time])
        .box_width(width);
        let b = Bar::new(time.timestamp() as f64 / divider, volume)
            .fill(red)
            .vertical()
            .stroke(Stroke::new(1.0, red))
            .width(width);
        return (bb, b);
    } else {
        let bb: BoxElem = BoxElem::new(
            (time.timestamp() as f64) / divider,
            BoxSpread::new(low, open, a1, close, high),
        )
        .whisker_width(0.0)
        .fill(green)
        .stroke(Stroke::new(2.0, green))
        .name(format!["{}", time])
        .box_width(width);
        let b = Bar::new(time.timestamp() as f64 / divider, volume)
            .fill(green)
            .vertical()
            .stroke(Stroke::new(1.0, green))
            .width(width);
        return (bb, b);
    }
}

macro_rules! make_p2{
    ( $($name:ident, $formatter:ident, $formatter2:ident, $y_lower:ident, $y_upper:ident, $x_lower:ident, $x_higher:ident, $v_higher:ident),* ) => {
        {
            let id=format!["{}",format!["plot_id_{}",$($name.to_string())*]];
            let axis_hints=AxisHints::new_y();
            axis_hints.clone().placement(HPlacement::Right);
            //TODO - to togle percentage change the x axis formatter
            //TODO - find a way to place the chart labels on the right... the above obviously
            let candle_plot = Plot::new($($name.to_string())*)
                .legend(Legend::default())
                .link_cursor(id.clone(), [true,false])
                .link_axis(id.clone(), [true,false])
                .width(560.0)
                .height(250.0)
                .custom_x_axes(vec![])
                .custom_y_axes(vec![])
                .x_axis_formatter($($formatter)*)
                .default_y_bounds($($y_lower)*,$($y_upper)*)
                .default_x_bounds($($x_lower)*,$($x_higher)*);
            let volume_plot = Plot::new(format!["{}_volume",$($name.to_string())*])
                .legend(Legend::default())
                .link_cursor(id.clone(), [true,false])
                .link_axis(id.clone(), [true,false])
                .width(560.0)
                .height(80.0)
                .default_y_bounds(0.0,$($v_higher)*)
                .custom_y_axes(vec![])
                .x_axis_formatter($($formatter)*);

            (candle_plot,volume_plot)
        }
    };
}
fn make_plot(
    name: &str,
    intv: Intv,
    y_lower: f64,
    y_higher: f64,
    x_lower: f64,
    x_higher: f64,
    v_higher: f64,
) -> (Plot<'_>, Plot<'_>) {
    match intv {
        Intv::Min1 => make_p2!(
            name,
            x_format_1min,
            grid_spacer_1min,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Min3 => make_p2!(
            name,
            x_format_3min,
            grid_spacer_3min,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Min5 => make_p2!(
            name,
            x_format_5min,
            grid_spacer_5min,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Min15 => make_p2!(
            name,
            x_format_15min,
            grid_spacer_15min,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Min30 => make_p2!(
            name,
            x_format_30min,
            grid_spacer_30min,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Hour1 => make_p2!(
            name,
            x_format_1h,
            grid_spacer_1h,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Hour2 => make_p2!(
            name,
            x_format_2h,
            grid_spacer_2h,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Hour4 => make_p2!(
            name,
            x_format_4h,
            grid_spacer_4h,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Hour6 => make_p2!(
            name,
            x_format_6h,
            grid_spacer_6h,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Hour8 => make_p2!(
            name,
            x_format_8h,
            grid_spacer_8h,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Hour12 => make_p2!(
            name,
            x_format_12h,
            grid_spacer_12h,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Day1 => make_p2!(
            name,
            x_format_1d,
            grid_spacer_1d,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Day3 => make_p2!(
            name,
            x_format_3d,
            grid_spacer_3d,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Week1 => make_p2!(
            name,
            x_format_1w,
            grid_spacer_1w,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
        Intv::Month1 => make_p2!(
            name,
            x_format_1mo,
            grid_spacer_1mo,
            y_lower,
            y_higher,
            x_lower,
            x_higher,
            v_higher
        ),
    }
}

const M1_DIV: i64 = 60;
const M3_DIV: i64 = M1_DIV * 3;
const M5_DIV: i64 = M1_DIV * 5;
const M15_DIV: i64 = M1_DIV * 15;
const M30_DIV: i64 = M1_DIV * 30;
const H1_DIV: i64 = M1_DIV * 60;
const H2_DIV: i64 = H1_DIV * 2;
const H4_DIV: i64 = H1_DIV * 4;
const H6_DIV: i64 = H1_DIV * 6;
const H8_DIV: i64 = H1_DIV * 8;
const H12_DIV: i64 = H1_DIV * 12;
const D1_DIV: i64 = H1_DIV * 24;
const D3_DIV: i64 = D1_DIV * 3;
const W1_DIV: i64 = D1_DIV * 7;
const MO1_DIV: i64 = D1_DIV * 30; //30 is kind of a hack... but it works... so wtf...

const GAP: f64 = 45.0;
const EXTRA_GAP: f64 = 1.2;

const M1_GAP: f64 = GAP / (M1_DIV as f64);
const M3_GAP: f64 = (GAP * 3.0) / (M3_DIV as f64);
const M5_GAP: f64 = (GAP * 5.0) / (M5_DIV as f64);
const M15_GAP: f64 = (GAP * 15.0) / (M15_DIV as f64);
const M30_GAP: f64 = (GAP * 30.0) / (M30_DIV as f64);
const H1_GAP: f64 = (GAP * 60.0) / (H1_DIV as f64);
const H2_GAP: f64 = (GAP * 60.0 * 2.0) / (H2_DIV as f64);
const H4_GAP: f64 = (GAP * 60.0 * 4.0) / (H4_DIV as f64);
const H6_GAP: f64 = (GAP * 60.0 * 6.0) / (H6_DIV as f64);
const H8_GAP: f64 = (GAP * 60.0 * 8.0) / (H8_DIV as f64);
const H12_GAP: f64 = (GAP * 60.0 * 12.0) / (H12_DIV as f64);
const D1_GAP: f64 = (GAP * 60.0 * 24.0) / (D1_DIV as f64);
const D3_GAP: f64 = (EXTRA_GAP * GAP * 60.0 * 72.0) / (D3_DIV as f64);
const W1_GAP: f64 = (EXTRA_GAP * GAP * 60.0 * 24.0 * 7.0) / (W1_DIV as f64);
const MO1_GAP: f64 = (EXTRA_GAP * GAP * 60.0 * 24.0 * 30.0) / (MO1_DIV as f64);

fn get_chart_params(intv: &Intv) -> (f64, f64) {
    match intv {
        Intv::Min1 => (M1_DIV as f64, M1_GAP),
        Intv::Min3 => (M3_DIV as f64, M3_GAP),
        Intv::Min5 => (M5_DIV as f64, M5_GAP),
        Intv::Min15 => (M15_DIV as f64, M15_GAP),
        Intv::Min30 => (M30_DIV as f64, M30_GAP),
        Intv::Hour1 => (H1_DIV as f64, H1_GAP),
        Intv::Hour2 => (H2_DIV as f64, H2_GAP),
        Intv::Hour4 => (H4_DIV as f64, H4_GAP),
        Intv::Hour6 => (H6_DIV as f64, H6_GAP),
        Intv::Hour8 => (H8_DIV as f64, H8_GAP),
        Intv::Hour12 => (H12_DIV as f64, H12_GAP),
        Intv::Day1 => (D1_DIV as f64, D1_GAP),
        Intv::Day3 => (D3_DIV as f64, D3_GAP),
        Intv::Week1 => (W1_DIV as f64, W1_GAP),
        Intv::Month1 => (MO1_DIV as f64, MO1_GAP), //with reference to 1970 1,1 00:00 perhaps?
    }
}

//FIXME convert to macros or fork egui_plot library and make a better implementation yourself...
#[allow(unused)]
fn grid_spacer_1min(input: GridInput) -> [f64; 3] {
    [60.0, 60.0, 1.0]
}

#[allow(unused)]
fn x_format_1min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * M1_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_3min(input: GridInput) -> [f64; 3] {
    [60.0, 60.0, 1.0]
}

#[allow(unused)]
fn x_format_3min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * M3_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_5min(input: GridInput) -> [f64; 3] {
    [60.0, 60.0, 1.0]
}

#[allow(unused)]
fn x_format_5min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * M5_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_15min(input: GridInput) -> [f64; 3] {
    [15.0, 15.0, 1.0]
}

#[allow(unused)]
fn x_format_15min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * M15_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_30min(input: GridInput) -> [f64; 3] {
    [15.0, 15.0, 1.0]
}

#[allow(unused)]
fn x_format_30min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * M30_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_1h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

#[allow(unused)]
fn x_format_1h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * H1_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_2h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}
#[allow(unused)]
fn x_format_2h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * H2_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_4h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

#[allow(unused)]
fn x_format_4h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * H4_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}

#[allow(unused)]
fn grid_spacer_6h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

#[allow(unused)]
fn x_format_6h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * H6_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_8h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

#[allow(unused)]
fn x_format_8h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * H8_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_12h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

#[allow(unused)]
fn x_format_12h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * H12_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_1d(input: GridInput) -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

#[allow(unused)]
fn x_format_1d(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * D1_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_3d(input: GridInput) -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

#[allow(unused)]
fn x_format_3d(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * D3_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_1w(input: GridInput) -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

#[allow(unused)]
fn x_format_1w(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * W1_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
#[allow(unused)]
fn grid_spacer_1mo(input: GridInput) -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

#[allow(unused)]
fn x_format_1mo(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * MO1_DIV;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}

#[allow(unused)]
fn time_format(input: &BoxElem, plot: &BoxPlot) -> String {
    let red = Color32::from_rgb(255, 0, 0);
    let green = Color32::from_rgb(0, 255, 0);
    let (o, c) = match input.fill {
        red => (input.spread.quartile1, input.spread.quartile3),
        green => (input.spread.quartile3, input.spread.quartile1),
    };
    format!(
        "Time: {} \n Open: {} \n High: {} \n Low: {} \n Close: {} \n Average: {:.2}",
        input.name,
        o,
        input.spread.upper_whisker,
        input.spread.lower_whisker,
        c,
        input.spread.median
    )
}

#[derive(EnumIter, Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum PaneType {
    None,
    LiveTrade,
    HistTrade,
    ManageData,
    Settings,
}
impl fmt::Display for PaneType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PaneType::LiveTrade => write!(f, "Live Trade"),
            PaneType::HistTrade => write!(f, "Hist Trade"),
            PaneType::ManageData => write!(f, "Manage Data"),
            PaneType::Settings => write!(f, "Settings"),
            PaneType::None => write!(f, "None"),
        }
    }
}

#[derive(PartialEq, Dbg)]
struct Pane {
    nr: usize,
    ty: PaneType,
}

//TODO this is retarded but fine for now. The default method
impl Default for Pane {
    fn default() -> Self {
        Self {
            nr: 0,
            ty: PaneType::None,
        }
    }
}

impl Pane {
    pub fn new(nr: usize, ty: PaneType) -> Self {
        Self { nr, ty }
    }
}

impl egui_tiles::Behavior<Pane> for DesktopApp {
    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        format!("{}", pane.ty).into()
    }
    #[allow(unused)]
    fn top_bar_right_ui(
        &mut self,
        _tiles: &egui_tiles::Tiles<Pane>,
        ui: &mut egui::Ui,
        tile_id: egui_tiles::TileId,
        _tabs: &egui_tiles::Tabs,
        _scroll_offset: &mut f32,
    ) {
        self.add_child_to = Some(tile_id);
    }
    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        self.simplification_options
    }
    fn is_tab_closable(&self, _tiles: &Tiles<Pane>, _tile_id: TileId) -> bool {
        true
    }
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: egui_tiles::TileId,
        pane: &mut Pane,
    ) -> egui_tiles::UiResponse {
        let response: egui_tiles::UiResponse;
        if ui
            .add(egui::Button::new("").sense(egui::Sense::drag()))
            .drag_started()
        {
            response = egui_tiles::UiResponse::DragStarted
        } else {
            response = egui_tiles::UiResponse::None
        }
        let color = egui::epaint::Hsva::new(0.0, 0.0, 0.0, 0.0);
        ui.painter().rect_filled(ui.max_rect(), 0.0, color);
        match pane.ty {
            PaneType::None => {}
            PaneType::LiveTrade => {
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");
                let live_price = self.live_price.lock().expect("Live price mutex poisoned!");
                let live_plot_l = self.live_plot.clone();
                let live_info_l = self.live_info.clone();
                let c_data = self
                    .collect_data
                    .lock()
                    .expect("Live collect data mutex poisoned!");

                let p_extras: PlotExtras = PlotExtras::None;
                let mut live_plot = live_plot_l.lock().expect("Live plot mutex posoned!");
                let mut live_info = live_info_l.lock().expect("Live plot mutex posoned!");

                LivePlot::show(
                    &mut live_plot,
                    &p_extras,
                    chan,
                    &live_price,
                    &c_data,
                    ui,
                    &mut live_info,
                );
                let mut man_orders = self
                    .man_orders
                    .get_mut(&pane.nr)
                    .expect("Manual order window not found!");
                if man_orders.plot_extras.is_some() != true {
                    man_orders.plot_extras = Some(p_extras);
                };
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");
                let _res = ManualOrders::show(
                    &mut man_orders,
                    &live_price,
                    None,
                    chan,
                    ui,
                    Some(&mut live_plot.kline_plot.hlines),
                    Some(&live_info),
                    None,
                    None,
                );
            }
            PaneType::HistTrade => {
                match self.resp_buff.as_ref() {
                    Some(a) => a.get(&ProcResp::SQLResp(SQLResponse::None)),
                    None => None,
                };
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");
                let ts = self.trade_slice.clone();

                let mut t_slice = ts.lock().expect("Trade slice!");

                let mut hist_extras = self
                    .hist_extras
                    .get_mut(&pane.nr)
                    .expect("Hist plot extras struct not found!");
                let p_extras: PlotExtras = PlotExtras::None;
                let mut h_plot = self
                    .hist_plot
                    .get_mut(&pane.nr)
                    .expect("Hist plot gui struct not found!");

                HistPlot::show(
                    &mut h_plot,
                    &p_extras,
                    chan,
                    self.hist_asset_data.clone(),
                    ui,
                    Some(&mut t_slice),
                    &mut hist_extras,
                );

                let mut man_orders = self
                    .man_orders
                    .get_mut(&pane.nr)
                    .expect("Manual order window not found!");
                if man_orders.plot_extras.is_some() != true {
                    man_orders.plot_extras = Some(p_extras);
                };
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");
                let _res = ManualOrders::show(
                    &mut man_orders,
                    &hist_extras.last_price,
                    Some(&mut h_plot.hist_trade),
                    chan,
                    ui,
                    Some(&mut h_plot.kline_plot.hlines),
                    None,
                    Some(&t_slice),
                    Some(&hist_extras.symbol_info),
                );
            }
            PaneType::ManageData => {
                match self.resp_buff.as_ref() {
                    Some(a) => a.get(&ProcResp::SQLResp(SQLResponse::None)),
                    None => None,
                };
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");

                let data_manager_l = self.data_manager.clone();
                let mut data_manager = data_manager_l.lock().expect("Data manager mutex posoned!");

                DataManager::show(&mut data_manager, chan, ui);
            }
            PaneType::Settings => {
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");

                let settings_l = self.settings.clone();
                let mut settings = settings_l.lock().expect("Data manager mutex posoned!");

                let live_inf = self.live_info.clone();
                let live_info = live_inf.lock().expect("Unalle to unlock live_info mutex");

                Settings::show(&mut settings, &live_info, chan, ui);
            }
        };
        return response;
    }
    fn on_tab_close(
        //NOTE - remove old structs form maps here...
        &mut self,
        tiles: &mut Tiles<Pane>,
        tile_id: TileId,
    ) -> bool {
        if let Some(tile) = tiles.get(tile_id) {
            match tile {
                Tile::Pane(pane) => {
                    match pane.ty {
                        PaneType::None => {}
                        PaneType::LiveTrade => {
                            self.man_orders.remove(&pane.nr);
                        }
                        PaneType::HistTrade => {
                            self.hist_plot.remove(&pane.nr);
                            self.man_orders.remove(&pane.nr);
                            self.hist_extras.remove(&pane.nr);
                        }
                        PaneType::ManageData => {}
                        PaneType::Settings => {}
                    }
                    let tab_title = self.tab_title_for_pane(pane);
                    tracing::trace!("Closing tab: {}, tile ID: {tile_id:?}", tab_title.text());
                }
                Tile::Container(container) => {
                    tracing::trace!("Closing container: {:?}", container.kind());
                    let children_ids = container.children();
                    for child_id in children_ids {
                        if let Some(Tile::Pane(pane)) = tiles.get(*child_id) {
                            let tab_title = self.tab_title_for_pane(pane);
                            tracing::trace!(
                                "Closing tab: {}, tile ID: {tile_id:?}",
                                tab_title.text()
                            );
                        }
                    }
                }
            }
        }
        true
    }
}

fn create_tree() -> egui_tiles::Tree<Pane> {
    let gen_pane = || {
        let pane = Pane {
            nr: 0,
            ty: PaneType::LiveTrade,
        };
        pane
    };
    let mut tiles = egui_tiles::Tiles::default();
    let mut tabs = vec![];
    tabs.push(tiles.insert_pane(gen_pane()));
    let root = tiles.insert_tab_tile(tabs);
    egui_tiles::Tree::new("my_tree", root, tiles)
}

#[allow(unused)]
#[derive(PartialEq, Debug, Clone)]
enum PlotExtras {
    None,
    OrderHlines(Vec<HLine>),
    TradeSlice(Vec<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>),
}

#[allow(unused)]
#[derive(PartialEq, Debug, Clone, Default)]
pub struct LiveInfo {
    pub live_asset_symbol_changed: (bool, String),
    pub acc_balances: HashMap<String, (f64, f64)>,
    pub current_pair_strings: (String, String),
    pub current_pair_free_balances: (f64, f64),
    pub current_pair_locked_balances: (f64, f64),
    pub live_orders: HashMap<i32, (Order, bool)>,
    pub keys_status: KeysStatus,
}

#[derive(Dbg)]
pub struct DesktopApp {
    trade_slice: Rc<Mutex<Vec<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>>>,

    simplification_options: egui_tiles::SimplificationOptions,
    tab_bar_height: f32,
    gap_width: f32,
    add_child_to: Option<egui_tiles::TileId>,

    pane_number: usize,
    #[dbg(skip)]
    tree: Rc<Mutex<egui_tiles::Tree<Pane>>>,

    label: String,

    value: f32,
    lock_x: bool,
    lock_y: bool,
    ctrl_to_zoom: bool,
    shift_to_horizontal: bool,
    zoom_speed: f32,
    scroll_speed: f32,

    live_price: Arc<Mutex<f64>>,
    asset_data: Arc<Mutex<AssetData>>,
    hist_asset_data: Arc<Mutex<AssetData>>,
    collect_data: Arc<Mutex<HashMap<String, SymbolOutput>>>,

    //Non-copy windows
    live_plot: Rc<Mutex<LivePlot>>,
    data_manager: Rc<Mutex<DataManager>>,
    settings: Rc<Mutex<Settings>>,
    live_info: Arc<Mutex<LiveInfo>>,

    lp_chan_recv: watch::Receiver<f64>,

    send_to_cli: Option<watch::Sender<ClientInstruct>>,
    recv_from_cli: Option<watch::Receiver<ClientResponse>>,

    resp_buff: Option<HashMap<ProcResp, Vec<ClientResponse>>>,
    last_resp: Option<ClientResponse>,

    man_orders: BTreeMap<usize, ManualOrders>,
    hist_plot: BTreeMap<usize, HistPlot>,
    hist_extras: BTreeMap<usize, HistExtras>,
}

impl DesktopApp {
    fn update_procresp(
        last_resp: &mut ClientResponse,
        recv: &mut watch::Receiver<ClientResponse>,
        buff: &mut HashMap<ProcResp, Vec<ClientResponse>>,
    ) {
        let resp = recv.borrow_and_update().clone();
        if &resp != last_resp {
            match resp {
                ClientResponse::None => {}
                ClientResponse::Success | ClientResponse::Failure(_) => {
                    let res = buff.get(&ProcResp::Client);
                    match res {
                        Some(_) => {
                            let resp_vec = buff
                                .get_mut(&ProcResp::Client)
                                .expect("Unable to find cli_resp vector!");
                            resp_vec.push(resp.clone())
                        }
                        None => {
                            buff.insert(ProcResp::Client, vec![]);
                        }
                    }
                }
                ClientResponse::ProcResp(ref proc_resp) => {
                    let res = buff.get(&proc_resp);
                    match res {
                        Some(_) => {
                            let resp_vec = buff
                                .get_mut(&proc_resp)
                                .expect("Unable to find proc_resp vector!");
                            resp_vec.push(resp.clone());
                        }
                        None => {
                            buff.insert(proc_resp.clone(), vec![]);
                        }
                    }
                }
            }
            *last_resp = resp;
        }
    }
}

impl Default for DesktopApp {
    fn default() -> Self {
        // NOTE this struct should never be initiated this way, all refs need to be passed
        //
        let lp = Arc::new(Mutex::new(0.0));
        let asset_data = Arc::new(Mutex::new(AssetData::new(6661)));
        let hist_asset_data = Arc::new(Mutex::new(AssetData::new(6662)));
        let cd = Arc::new(Mutex::new(HashMap::new()));
        let trade_slice = Rc::new(Mutex::new(vec![]));

        let (_, lp_chan_recv) = watch::channel(0.0);

        let man_o = ManualOrders::default();
        let mut man_o_default = BTreeMap::new();
        man_o_default.insert(0, man_o);

        Self {
            trade_slice,

            last_resp: Some(ClientResponse::None),
            resp_buff: Some(HashMap::new()),
            simplification_options: egui_tiles::SimplificationOptions {
                ..egui_tiles::SimplificationOptions::OFF
            },
            tab_bar_height: 24.0,
            gap_width: 2.0,
            add_child_to: None,

            tree: Rc::new(Mutex::new(create_tree())),
            pane_number: 1,

            label: "Bintrade".to_owned(),
            value: 2.7,
            lock_x: false,
            lock_y: false,
            ctrl_to_zoom: false,
            shift_to_horizontal: false,
            zoom_speed: 1.0, //
            scroll_speed: 1.0,

            //Cli refs
            live_price: lp,
            asset_data,
            hist_asset_data: hist_asset_data.clone(),
            collect_data: cd,

            lp_chan_recv,

            live_plot: Rc::new(Mutex::new(LivePlot::default())),
            data_manager: Rc::new(Mutex::new(DataManager::new(hist_asset_data))),

            settings: Rc::new(Mutex::new(Settings::new())),
            live_info: Arc::new(Mutex::new(LiveInfo::default())),

            //Channels
            send_to_cli: None,
            recv_from_cli: None,

            man_orders: man_o_default,
            hist_plot: BTreeMap::new(),
            hist_extras: BTreeMap::new(),
        }
    }
}

impl DesktopApp {
    #[allow(unused)]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        schan: watch::Sender<ClientInstruct>,
        rchan: watch::Receiver<ClientResponse>,
        asset_data: Arc<Mutex<AssetData>>,
        hist_asset_data: Arc<Mutex<AssetData>>,
        live_price: Arc<Mutex<f64>>,
        collect_data: Arc<Mutex<HashMap<String, SymbolOutput>>>,
        settings: Settings,
        live_info: Arc<Mutex<LiveInfo>>,
    ) -> Self {
        let live_plot = Rc::new(Mutex::new(LivePlot::new(
            asset_data.clone(),
            &settings.default_intv,
            &settings.default_asset,
        )));
        let data_manager = Rc::new(Mutex::new(DataManager::new(hist_asset_data.clone())));
        let settings = Rc::new(Mutex::new(settings));
        Self {
            send_to_cli: Some(schan),
            recv_from_cli: Some(rchan),
            live_price,
            live_plot,
            collect_data,
            asset_data,
            hist_asset_data,
            data_manager,
            settings,
            live_info,
            ..Default::default()
        }
    }
    #[allow(unused)]
    pub fn new_testing(cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            ..Default::default()
        }
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.heading("Bintrade_egui 0.1.0");
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Load settings").clicked() {
                        //TODO - save settings  to file and load them
                    }
                    if ui.button("Save settings").clicked() {}
                    if ui.button("Quit").clicked() {
                        let _chan = match self.send_to_cli.clone() {
                            Some(chan) => {
                                let msg = ClientInstruct::Terminate;
                                let _res = chan.send(msg);
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            None => {
                                tracing::error!["Gui send to cliL None"]
                            }
                        };
                    }
                });
                ui.add_space(16.0);
                let mut next_panel_type: PaneType = PaneType::None;
                ComboBox::from_label("")
                    .selected_text(format!("Tools"))
                    .show_ui(ui, |ui| {
                        for pty in PaneType::iter() {
                            ui.selectable_value(&mut next_panel_type, pty, format!["{}", pty]);
                        }
                    });
                if next_panel_type != PaneType::None {
                    if let Some(parent) = self.add_child_to.take() {
                        let mut tree = self.tree.lock().expect("Posoned mutex on pane tree!");
                        let mut new_child = tree
                            .tiles
                            .insert_pane(Pane::new(self.pane_number, PaneType::None));
                        match next_panel_type {
                            PaneType::None => {}
                            PaneType::LiveTrade => {
                                self.man_orders
                                    .insert(self.pane_number + 1, ManualOrders::default());

                                new_child = tree.tiles.insert_pane(Pane::new(
                                    self.pane_number + 1,
                                    PaneType::LiveTrade,
                                ));
                                self.pane_number += 1;
                            }
                            PaneType::HistTrade => {
                                self.man_orders
                                    //FIXME add CTX here
                                    .insert(self.pane_number + 1, ManualOrders::default());
                                let settings =
                                    self.settings.lock().expect("Unable to unlock settings");
                                self.hist_plot.insert(
                                    self.pane_number + 1,
                                    HistPlot::new(
                                        Arc::clone(&self.hist_asset_data),
                                        &settings.default_intv,
                                        settings.defalt_next_wicks,
                                    ),
                                );
                                self.hist_extras
                                    .insert(self.pane_number + 1, HistExtras::default());

                                new_child = tree.tiles.insert_pane(Pane::new(
                                    self.pane_number + 1,
                                    PaneType::HistTrade,
                                ));
                                self.pane_number += 1;
                            }
                            PaneType::ManageData => {
                                new_child = tree.tiles.insert_pane(Pane::new(
                                    self.pane_number + 1,
                                    PaneType::ManageData,
                                ));
                                self.pane_number += 1;
                            }
                            PaneType::Settings => {
                                new_child = tree.tiles.insert_pane(Pane::new(
                                    self.pane_number + 1,
                                    PaneType::Settings,
                                ));
                                self.pane_number += 1;
                            }
                        }
                        if let Some(egui_tiles::Tile::Container(egui_tiles::Container::Tabs(
                            tabs,
                        ))) = tree.tiles.get_mut(parent)
                        {
                            tabs.add_child(new_child);
                            tabs.set_active(new_child);
                        }
                    }
                }
                ui.add_space(16.0);
                egui::widgets::global_theme_preference_buttons(ui);
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut last_resp = self.last_resp.take().expect("Unable to get last_resp");
            let mut recv = self
                .recv_from_cli
                .take()
                .expect("Unable to get recv_from_client channel!");
            let mut buff = self.resp_buff.take().expect("Unable to get resp_buff");
            DesktopApp::update_procresp(&mut last_resp, &mut recv, &mut buff);
            self.recv_from_cli = Some(recv);
            self.resp_buff = Some(buff);
            self.last_resp = Some(last_resp);
            let tt = self.tree.clone();
            let mut tree = tt.lock().expect("Posoned mutex on pane tree!");
            tree.ui(self, ui);
        });
    }
}

#[derive(Dbg, Clone)]
struct ManualOrders {
    man_orders: Option<HistTrade>,
    active_orders: Option<Vec<Order>>,

    hotkeys: bool,

    current_symbol: String,

    search_string: String,
    quant: Quant,
    quant_selector: Quant,
    last_quant: Quant,

    order: Order,
    new_order: Order,
    scalar_set: bool,
    scalar: f64,
    buy: bool,
    price_string: String,
    stop_price_string: String,
    orders: HashMap<i32, (Order, bool)>,
    asset1: f64,
    asset2: f64,
    last_id: i32,
    asset1_name: String,
    asset2_name: String,

    price: f64,
    stop_price: f64,

    last_price_buffer: Vec<f64>,
    last_price_buffer_size: usize,
    last_price_s: f64,

    plot_extras: Option<PlotExtras>,
}

//TODO find a way to group GUI elements together. The horror...
impl Default for ManualOrders {
    fn default() -> Self {
        Self {
            man_orders: None,
            active_orders: None,
            hotkeys: false,

            current_symbol: "BTCUSDT".to_string(),

            search_string: "".to_string(),
            quant: Quant::Q100,
            last_quant: Quant::Q100,
            quant_selector: Quant::Q100,
            order: Order::Market {
                buy: true,
                quant: Quant::Q100,
            },
            new_order: Order::Market {
                buy: true,
                quant: Quant::Q100,
            },
            scalar: 100.0,
            scalar_set: false,
            buy: true,

            price_string: "0.0".to_string(),
            stop_price_string: "0.0".to_string(),
            orders: HashMap::new(),
            //NOTE if live replace hashmap with an arc mutex to hte clientshit
            asset1: 0.0,
            asset2: 0.0,
            asset1_name: "".to_string(),
            asset2_name: "".to_string(),
            last_price_buffer: vec![],
            last_price_buffer_size: 20,
            last_price_s: 0.0,
            last_id: 0,

            stop_price: 0.0,
            price: 0.0,

            plot_extras: None,
        }
    }
}

#[instrument(level = "trace")]
fn link_hline_orders(orders: &HashMap<i32, (Order, bool)>, hlines: &mut Vec<HLine>) {
    hlines.clear();
    let _ = orders
        .iter()
        .map(|(_, (order, active))| {
            let mut hh = HlineType::hline_order(order, *active);
            hlines.append(&mut hh);
        })
        .collect::<Vec<_>>();
}

#[allow(unused)]
impl ManualOrders {
    fn hist_validate_order(o: &Order, asset1: &f64, asset2: &f64) -> Result<()> {
        todo!()
    }
    fn new(man_orders: HistTrade) -> Self {
        Self {
            man_orders: Some(man_orders),
            ..Default::default()
        }
    }
    fn check_order_hist(&self, order: &Order, hist_trade: &mut HistTrade) -> Result<()> {
        //todo!()
        Ok(())
    }
    fn check_order_price(&self, order: &Order, last_price: &f64) -> Result<()> {
        Ok(())
        //todo!()
    }
    fn sync(&mut self, hist_trade: Option<&HistTrade>) {
        todo!()
    }
    fn show(
        mut man_orders: &mut ManualOrders,
        last_price: &f64,
        mut hist_trade: Option<&mut HistTrade>,
        cli_chan: watch::Sender<ClientInstruct>,
        ui: &mut egui::Ui,
        hlines: Option<&mut Vec<HLine>>,
        live_info: Option<&LiveInfo>,
        trade_slice: Option<&[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)]>,
        symbol_info: Option<&(String, String, String)>,
    ) {
        match hlines {
            Some(hline_ref) => link_hline_orders(&man_orders.orders, hline_ref),
            None => {
                tracing::debug!["Hlines not available!"];
            }
        };
        tracing::trace!["Manual orders last_price {}", &last_price];
        if let Some(live_inf) = live_info {
            man_orders.asset1_name = live_inf.current_pair_strings.0.clone();
            man_orders.asset2_name = live_inf.current_pair_strings.1.clone();
            man_orders.asset1 = live_inf.current_pair_free_balances.0;
            man_orders.asset2 = live_inf.current_pair_free_balances.1;
        };
        match (hist_trade, trade_slice) {
            (Some(mut h_trade), Some(t_slice)) => {
                man_orders.asset1 = h_trade.asset1;
                man_orders.asset2 = h_trade.asset2;
                if let Some(symbol_inf) = symbol_info {
                    man_orders.current_symbol = symbol_inf.0.clone();
                    man_orders.asset1_name = symbol_inf.1.clone();
                    man_orders.asset2_name = symbol_inf.2.clone();
                };
                //NOTE if the slice is long the evaluation might brick perormance
                let active_orders: Vec<(i32, Order)> = man_orders
                    .orders
                    .iter()
                    .filter(|(_, (_, active))| *active == true)
                    .map(|(id, (order, active))| (*id, *order))
                    .collect();
                let inactive_orders: Vec<(i32, Order)> = man_orders
                    .orders
                    .iter()
                    .filter(|(_, (_, active))| *active == false)
                    .map(|(id, (order, active))| (*id, *order))
                    .collect();
                let remaining_active_orders = h_trade.trade_forward(t_slice, 0, active_orders);
                let mut remaining_orders: HashMap<i32, (Order, bool)> = inactive_orders
                    .iter()
                    .map(|(id, order)| (*id, (*order, false)))
                    .collect();
                let _res: Vec<_> = remaining_active_orders
                    .iter()
                    .map(|(id, order)| {
                        remaining_orders.insert(*id, (*order, true));
                    })
                    .collect();
                man_orders.orders = remaining_orders;
            }
            (None, None) => {}
            _ => {
                tracing::error!["This shouldn't happen manual_orders"]
            }
        };
        egui::Grid::new("Current symboll:")
            .striped(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    /*
                    ui.label(
                        RichText::new(format!["Current asset: {}", man_orders.current_symbol])
                            .color(Color32::WHITE),
                    );
                    */
                    ui.end_row();
                });
            });
        ui.end_row();
        egui::Grid::new("Man order assets:")
            .striped(true)
            .show(ui, |ui| {
                ui.add_sized(
                    egui::vec2(50.0, 20.0),
                    egui::Label::new(
                        RichText::new(format!["{}:{}", man_orders.asset1_name, man_orders.asset1])
                            .color(Color32::from_rgb(255, 207, 38)),
                    ),
                );
                ui.add_sized(
                    egui::vec2(50.0, 20.0),
                    egui::Label::new(
                        RichText::new(format!["{}:{}", man_orders.asset2_name, man_orders.asset2])
                            .color(Color32::from_rgb(71, 200, 38)),
                    ),
                );
                ui.end_row();
            });
        ui.end_row();
        egui::Grid::new("Last price:").striped(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!["Last price: {}", last_price]).color(Color32::WHITE),
                );
                ui.checkbox(&mut man_orders.hotkeys, "Hotkeys");
                /*NOTE the bellow doesn't work, FIXME
                ui.end_row();
                man_orders.last_price_s = *last_price;
                man_orders.last_price_buffer.push(last_price);
                let n = man_orders.last_price_buffer.len();
                if n < man_orders.last_price_buffer_size {
                    ui.label(
                        RichText::new(format!["Last price: {}", last_price]).color(Color32::WHITE),
                    );
                } else {
                    let sum: f64 = man_orders.last_price_buffer.iter().sum();
                    if sum / (n as f64) <= *last_price {
                        ui.label(
                            RichText::new(format!["Last price: {}", last_price])
                                .color(Color32::GREEN),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!["Last price: {}", last_price])
                                .color(Color32::RED),
                        );
                    }
                }
                */
            });
        });
        ui.end_row();
        ui.ctx().request_repaint();
        if man_orders.hotkeys == true {
            /*
            if ui.ctx().input(|i| i.key_pressed(egui::Key::A)) {
                println!("\n A Pressed");
            }
            if ui.ctx().input(|i| i.key_down(egui::Key::A)) {
                println!("\n A Held");
                ui.ctx().request_repaint(); // make sure we note the holding.
            }
            if ui.ctx().input(|i| i.key_released(egui::Key::A)) {
                println!("\n A Released");
            }
            */
        };
        ui.end_row();

        egui::Grid::new("parent grid").striped(true).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.end_row();

                ui.end_row();
                let mut dummy: Option<f64> = None;
                ui.style_mut().visuals.selection.bg_fill = Color32::from_rgb(40, 40, 40);

                ui.horizontal(|ui| {
                    ui.selectable_value(&mut man_orders.quant_selector, Quant::Q25, "25%");
                    ui.selectable_value(&mut man_orders.quant_selector, Quant::Q50, "50%");
                    ui.selectable_value(&mut man_orders.quant_selector, Quant::Q75, "75%");
                    ui.selectable_value(&mut man_orders.quant_selector, Quant::Q100, "100%");
                });
                ui.end_row();
                if (man_orders.quant_selector != man_orders.last_quant) {
                    man_orders.quant = man_orders.quant_selector;
                    man_orders.last_quant = man_orders.quant_selector;
                    match man_orders.quant {
                        Quant::Q25 => man_orders.scalar = 25.0,
                        Quant::Q50 => man_orders.scalar = 50.0,
                        Quant::Q75 => man_orders.scalar = 75.0,
                        Quant::Q100 => man_orders.scalar = 100.0,
                        _ => (),
                    };
                } else {
                    match man_orders.scalar {
                        0.0 => (),
                        25.0 => {
                            man_orders.quant = Quant::Q25;
                            man_orders.quant_selector = Quant::Q25;
                            man_orders.last_quant = man_orders.quant_selector;
                            man_orders.scalar = 25.0;
                        }
                        50.0 => {
                            man_orders.quant = Quant::Q50;
                            man_orders.quant_selector = Quant::Q50;
                            man_orders.scalar = 50.0;
                            man_orders.last_quant = man_orders.quant_selector;
                        }
                        75.0 => {
                            man_orders.quant = Quant::Q75;
                            man_orders.quant_selector = Quant::Q75;
                            man_orders.scalar = 75.0;
                            man_orders.last_quant = man_orders.quant_selector;
                        }
                        100.0 => {
                            man_orders.quant = Quant::Q100;
                            man_orders.quant_selector = Quant::Q100;
                            man_orders.scalar = 100.0;
                            man_orders.last_quant = man_orders.quant_selector;
                        }
                        _ => {
                            man_orders.quant = Quant::Q {
                                q: man_orders.scalar,
                            };
                            man_orders.quant_selector = Quant::Q {
                                q: man_orders.scalar,
                            };
                        }
                    };
                };

                ui.add(egui::Slider::new(&mut man_orders.scalar, 0.0..=100.0).suffix(format!("%")));
                ui.end_row();
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut man_orders.buy,
                        true,
                        RichText::new("Buy").color(Color32::GREEN),
                    );
                    ui.selectable_value(
                        &mut man_orders.buy,
                        false,
                        RichText::new("Sell").color(Color32::RED),
                    );
                });
                ui.end_row();
                let bb = man_orders.buy;
                let qq = man_orders.quant;
                let sp = man_orders.stop_price;
                let pp = man_orders.price;

                egui::ComboBox::from_label("Order Type")
                    .selected_text(man_orders.order.to_str())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut man_orders.new_order,
                            Order::Market { buy: bb, quant: qq },
                            "Market",
                        );
                        ui.selectable_value(
                            &mut man_orders.new_order,
                            Order::Limit {
                                buy: bb,
                                quant: qq,
                                price: pp,
                                limit_status: LimitStatus::Untouched,
                            },
                            "Limit",
                        );
                        ui.selectable_value(
                            &mut man_orders.new_order,
                            Order::StopLimit {
                                buy: bb,
                                quant: qq,
                                price: pp,
                                limit_status: LimitStatus::Untouched,
                                stop_price: sp as f32,
                                stop_status: StopStatus::Untouched,
                            },
                            "Stop Limit",
                        );
                        ui.selectable_value(
                            &mut man_orders.new_order,
                            Order::StopMarket {
                                buy: bb,
                                quant: qq,
                                price: sp,
                                stop_status: StopStatus::Untouched,
                            },
                            "Stop Market",
                        );
                    });
                ui.end_row();
                match man_orders.new_order {
                    Order::Market { buy: _, quant: _ } => {
                        man_orders.order = Order::Market { buy: bb, quant: qq };
                    }
                    Order::Limit {
                        buy: _,
                        quant: _,
                        price: _,
                        limit_status: _,
                    } => {
                        ui.label("Enter the limit price:");
                        ui.add(
                            egui::TextEdit::singleline(&mut man_orders.price_string)
                                .hint_text("Enter the limit price"),
                        );
                        man_orders.order = Order::Limit {
                            buy: bb,
                            quant: qq,
                            price: pp,
                            limit_status: LimitStatus::Untouched,
                        };
                    }
                    Order::StopLimit {
                        buy: _,
                        quant: _,
                        price: _,
                        limit_status: _,
                        stop_price: _,
                        stop_status: _,
                    } => {
                        ui.label("Enter the limit price:");
                        ui.add(
                            egui::TextEdit::singleline(&mut man_orders.price_string)
                                .hint_text("Enter the limit price"),
                        );
                        ui.label("Enter the stop price:");
                        ui.add(
                            egui::TextEdit::singleline(&mut man_orders.stop_price_string)
                                .hint_text("Enter the stop price"),
                        );
                        man_orders.order = Order::StopLimit {
                            buy: bb,
                            quant: qq,
                            price: pp,
                            limit_status: LimitStatus::Untouched,
                            stop_price: sp as f32,
                            stop_status: StopStatus::Untouched,
                        };
                    }
                    Order::StopMarket {
                        buy: _,
                        quant: _,
                        price: _,
                        stop_status: _,
                    } => {
                        ui.label("Enter the stop price:");
                        ui.add(
                            egui::TextEdit::singleline(&mut man_orders.stop_price_string)
                                .hint_text("Enter the stop price"),
                        );
                        man_orders.order = Order::StopMarket {
                            buy: bb,
                            quant: qq,
                            price: pp,
                            stop_status: StopStatus::Untouched,
                        };
                    }
                    Order::None => {}
                }

                if ui.button("Add").clicked() {
                    let res = man_orders.price_string.parse();
                    man_orders.price = match res {
                        Ok(pp) => pp,
                        Err(e) => {
                            tracing::error!["Unable to parse price string!"];
                            man_orders.price_string = "0.0".to_string();
                            0.0
                        }
                    };
                    let res = man_orders.stop_price_string.parse();
                    man_orders.stop_price = match res {
                        Ok(sp) => sp,
                        Err(e) => {
                            tracing::error!["Unable to stop parse price string!"];
                            man_orders.stop_price_string = "0.0".to_string();
                            0.0
                        }
                    };
                    ///////////////
                    //FIXME neaten this up somehow
                    //
                    let bb = man_orders.buy;
                    let qq = man_orders.quant;
                    let sp = man_orders.stop_price;
                    let pp = man_orders.price;
                    match man_orders.new_order {
                        Order::Market { buy: _, quant: _ } => {
                            man_orders.order = Order::Market { buy: bb, quant: qq };
                        }
                        Order::Limit {
                            buy: _,
                            quant: _,
                            price: _,
                            limit_status: _,
                        } => {
                            man_orders.order = Order::Limit {
                                buy: bb,
                                quant: qq,
                                price: pp,
                                limit_status: LimitStatus::Untouched,
                            };
                        }
                        Order::StopLimit {
                            buy: _,
                            quant: _,
                            price: _,
                            limit_status: _,
                            stop_price: _,
                            stop_status: _,
                        } => {
                            man_orders.order = Order::StopLimit {
                                buy: bb,
                                quant: qq,
                                price: pp,
                                limit_status: LimitStatus::Untouched,
                                stop_price: sp as f32,
                                stop_status: StopStatus::Untouched,
                            };
                        }
                        Order::StopMarket {
                            buy: _,
                            quant: _,
                            price: _,
                            stop_status: _,
                        } => {
                            man_orders.order = Order::StopMarket {
                                buy: bb,
                                quant: qq,
                                price: pp,
                                stop_status: StopStatus::Untouched,
                            };
                        }
                        Order::None => {}
                    };
                    if let Some(live_inf) = live_info {
                        let o = man_orders.order;
                        let msg = ClientInstruct::SendBinInstructs(BinInstructs::PlaceOrder {
                            symbol: live_inf.live_asset_symbol_changed.1.clone(),
                            o: o.clone(),
                        });
                        let _res = cli_chan.send(msg);
                    } else {
                        let o = man_orders.order;
                        let order_valid_hist = ManualOrders::hist_validate_order(
                            &o,
                            &man_orders.asset1,
                            &man_orders.asset2,
                        );
                        match order_valid_hist {
                            Ok(_) => {
                                man_orders.last_id += 1;
                                let oid = man_orders.last_id;
                                man_orders.orders.insert(oid, (o, false));
                            }
                            Err(e) => {
                                tracing::error!["Hist order invalid ERROR: {}", e];
                            }
                        };
                    };
                };
                if let Some(live_inf) = live_info {
                    man_orders.orders = live_inf.live_orders.clone();

                    match live_inf.keys_status {
                        KeysStatus::NotAdded => {
                            ui.label(
                                RichText::new(format!["Add or unlock binance API keys",])
                                    .color(Color32::ORANGE),
                            );
                        }
                        KeysStatus::Invalid => {
                            ui.label(
                                RichText::new(format!["Api Keys Invalid",]).color(Color32::RED),
                            );
                        }
                        KeysStatus::Valid => {
                            ui.label(
                                RichText::new(format!["Api Keys Valid",]).color(Color32::GREEN),
                            );
                        }
                    };
                };

                /*
                if ui.button("Replace").clicked() {
                };
                */
                //FIXME find a way to non-duplicate above code
                ui.separator();
                ui.end_row();
            });
            ui.vertical(|ui| {
                let available_height = ui.available_height();
                let mut table = TableBuilder::new(ui)
                    .resizable(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::auto().resizable(false))
                    .column(Column::auto().resizable(false))
                    .column(
                        Column::remainder()
                            .at_least(40.0)
                            .clip(true)
                            .resizable(false),
                    )
                    .column(Column::auto().resizable(false))
                    .column(Column::auto().resizable(false))
                    .column(Column::auto().resizable(false))
                    .min_scrolled_height(0.0)
                    .max_scroll_height(available_height);
                table
                    .header(20.0, |mut header| {
                        header.col(|ui| {
                            ui.strong("ID");
                        });
                        header.col(|ui| {
                            ui.strong("Quantity");
                        });
                        header.col(|ui| {
                            ui.strong("Price");
                        });
                        header.col(|ui| {
                            ui.strong("Order Type");
                        });
                        header.col(|ui| {
                            ui.strong("Side");
                        });
                    })
                    .body(|mut body| {
                        let orders = man_orders.orders.clone();
                        for (id, (order, active)) in orders.iter() {
                            let row_height = 18.0;
                            body.row(row_height, |mut row| {
                                row.col(|ui| {
                                    ui.label(format!["{}", id]);
                                });
                                row.col(|ui| {
                                    ui.label(format!["{}", order.get_qnt() * man_orders.asset1]);
                                });
                                row.col(|ui| {
                                    ui.label(format!["{}", order.get_price()]);
                                });
                                row.col(|ui| {
                                    ui.label(order.to_str());
                                });
                                row.col(|ui| {
                                    ui.label(order.get_side_str());
                                });
                                row.col(|ui| {
                                    if ui.button("Delete").clicked() {
                                        man_orders.orders.remove(id);
                                    }
                                });
                            });
                        }
                    });
            });
        });
    }
}

#[derive(Dbg, Clone)]
struct LivePlot {
    live_asset_data: Arc<Mutex<AssetData>>,
    kline_plot: KlinePlot,
    search_string: String,
    symbol: String,
    default_symbol: String,
    intv: Intv,
    last_intv: Intv,
    lines: Vec<HLine>,
    reload: bool,
    last_symbol: String,
}

impl Default for LivePlot {
    fn default() -> Self {
        Self {
            kline_plot: KlinePlot::default(),
            live_asset_data: Arc::new(Mutex::new(AssetData::new(666))),

            search_string: "".to_string(),
            symbol: "BTCUSDT".to_string(),
            intv: Intv::Min1,
            last_intv: Intv::Min1,
            lines: vec![],
            reload: false,
            default_symbol: "BTCUSDT".to_string(),
            last_symbol: "BTCUSDT".to_string(),
        }
    }
}

impl LivePlot {
    fn new(
        live_asset_data: Arc<Mutex<AssetData>>,
        default_intv: &Intv,
        default_symbol: &str,
    ) -> Self {
        Self {
            live_asset_data,
            intv: *default_intv,
            last_intv: *default_intv,
            symbol: default_symbol.to_string(),
            default_symbol: default_symbol.to_string(),
            ..Default::default()
        }
    }
    #[allow(unused)]
    fn show(
        live_plot: &mut LivePlot,
        plot_extras: &PlotExtras,
        cli_chan: watch::Sender<ClientInstruct>,
        live_price: &f64,
        collect_data: &HashMap<String, SymbolOutput>,
        ui: &mut egui::Ui,
        live_info: &mut LiveInfo,
    ) {
        live_plot.kline_plot.show_live(
            ui,
            plot_extras,
            live_plot.live_asset_data.clone(),
            Some(collect_data),
            Some(live_info),
            None,
            None,
            None,
        );

        tracing::trace!["\x1b[36m Collected Data\x1b[0m: {:?}", &collect_data];

        ui.end_row();
        if live_plot.intv != live_plot.last_intv {
            live_plot.reload = true;
            tracing::trace!["\x1b[36m live chart reloaded\x1b[0m: "];

            live_plot.last_intv = live_plot.intv;
            live_plot.kline_plot.intv = live_plot.intv;
            live_plot.reload = false;
        };
        egui::Grid::new("Lplot order assets:")
            .striped(true)
            .show(ui, |ui| {
                egui::ComboBox::from_label("")
                    .selected_text(format!("{}", live_plot.intv.to_str()))
                    .show_ui(ui, |ui| {
                        for i in Intv::iter() {
                            ui.selectable_value(&mut live_plot.intv, i, i.to_str());
                        }
                    });
                let search = ui.add_sized(
                    egui::vec2(100.0, 20.0),
                    egui::TextEdit::singleline(&mut live_plot.search_string)
                        .hint_text("Search for asset"),
                );
                if ui.button("Search").clicked() {
                    if live_plot.kline_plot.live_asset_changed == false {
                        let msg = ClientInstruct::SendBinInstructs(BinInstructs::ChangeLiveAsset {
                            symbol: live_plot.search_string.clone(),
                            defualt_symbol: live_plot.default_symbol.clone(),
                        });
                        let _res = cli_chan.send(msg);
                    } else {
                        if live_plot.last_symbol != live_plot.kline_plot.symbol {
                            let msg =
                                ClientInstruct::SendBinInstructs(BinInstructs::ChangeLiveAsset {
                                    symbol: live_plot.search_string.clone(),
                                    defualt_symbol: live_plot.default_symbol.clone(),
                                });
                            let _res = cli_chan.send(msg);
                            live_plot.last_symbol = live_plot.kline_plot.symbol.clone();
                            live_plot.kline_plot.live_asset_changed = true;
                        };
                    };
                }
                /*
                if ui.button("Reload chart").clicked() {
                    //FIXME add a way to reload chart
                    //
                    //
                    //
                };
                */
            });
    }
}

#[derive(Clone, Dbg, Default)]
struct HistExtras {
    last_price: f64,
    symbol_info: (String, String, String),
}

#[derive(Dbg)]
struct HistPlot {
    hist_asset_data: Arc<Mutex<AssetData>>,
    kline_plot: KlinePlot,
    search_string: String,
    loaded_search_string: String,
    unload_search_string: String,

    search_date: String,
    search_load_string: String,
    intv: Intv,
    last_intv: Intv,
    picked_date: chrono::NaiveDate,
    picked_date_end: chrono::NaiveDate,

    trade_h: u16,
    trade_min: u16,

    trade_h_s: String,
    trade_min_s: String,

    hist_extras: Option<HistExtras>,
    hist_trade: HistTrade,

    navi_wicks: u16,
    trade_wicks: u16,

    navi_wicks_s: String,
    trade_wicks_s: String,

    pub trade_time: i64,
    pub last_trade_time: i64,

    pub att_klines: Option<Klines>,
    pub all_loaded: bool,

    pub trade_slice_loaded: bool,
}

impl Default for HistPlot {
    fn default() -> Self {
        let curr_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Unable to get unix timestamp!")
            .as_millis() as i64;
        let curr_date = chrono::NaiveDateTime::from_timestamp_millis(curr_timestamp)
            .expect("Unable to parse current time")
            .date();
        let current_year = chrono::Utc::now().year() as i32;
        Self {
            kline_plot: KlinePlot::default(),
            hist_asset_data: Arc::new(Mutex::new(AssetData::new(666))),

            search_string: "".to_string(),
            search_date: "".to_string(),
            unload_search_string: "".to_string(),
            loaded_search_string: "".to_string(),
            search_load_string: "".to_string(),

            intv: Intv::Min1,
            last_intv: Intv::Min1,

            picked_date: chrono::NaiveDate::from_ymd(current_year - 1, 1, 1),
            picked_date_end: curr_date,

            hist_extras: None,
            hist_trade: HistTrade::default(),

            navi_wicks: 200,
            trade_wicks: DEFAULT_TRADE_WICKS,

            navi_wicks_s: "200".to_string(),
            trade_wicks_s: "30".to_string(),

            trade_h: 0,
            trade_min: 0,

            trade_h_s: "00".to_string(),
            trade_min_s: "00".to_string(),

            trade_time: curr_date.and_hms(0, 0, 0).timestamp_millis(),
            last_trade_time: curr_date.and_hms(0, 0, 0).timestamp_millis(),

            att_klines: None,
            all_loaded: false,

            trade_slice_loaded: false,
        }
    }
}

#[derive(Dbg, Default, Encode, Decode, Clone, Eq, PartialEq, Copy)]
pub enum KeysStatus {
    #[default]
    NotAdded,
    Invalid,
    Valid,
}
#[allow(unused)]
impl KeysStatus {
    fn to_str(&self) -> String {
        match self {
            KeysStatus::NotAdded => "Keys not added".to_string(),
            KeysStatus::Invalid => "Keys invalid".to_string(),
            KeysStatus::Valid => "Keys valid".to_string(),
        }
    }
}

#[derive(Dbg, Default, Encode, Decode, Clone, PartialEq)]
pub struct Settings {
    pub enc_pub_key: Vec<u8>,
    pub enc_priv_key: Vec<u8>,

    pub api_key_enter_string: String,
    pub priv_api_key_enter_string: String,

    pub default_asset: String,
    pub default_intv: Intv,
    pub enc_api_keys: bool,
    pub save_api_keys: bool,
    pub defalt_next_wicks: i64,

    pub binance_pub_key: Option<String>,
    pub binance_priv_key: Option<String>,

    pub enc_binance_pub_key: Option<String>,
    pub enc_binance_priv_key: Option<String>,

    pub password_string: String,
    pub backload_wicks: usize,

    pub key_status: KeysStatus,

    pub balances: HashMap<String, (f64, f64)>,
}

#[allow(unused)]
impl Settings {
    pub fn new() -> Self {
        Settings {
            default_asset: "BTCUSDT".to_string(),
            enc_api_keys: true,
            save_api_keys: true,
            defalt_next_wicks: 30,
            backload_wicks: 740,
            ..Default::default()
        }
    }
    pub fn verify_password_req(password: &str) -> bool {
        if password.len() < 15 {
            return false;
        };
        //FIXME add more requirements
        true
    }
    #[allow(unused)]
    fn show(
        settings: &mut Settings,
        live_info: &LiveInfo,
        cli_chan: watch::Sender<ClientInstruct>,
        ui: &mut egui::Ui,
    ) {
        settings.balances = live_info.acc_balances.clone();
        settings.key_status = live_info.keys_status;

        egui::Grid::new("Account")
            .striped(true)
            .min_col_width(30.0)
            .show(ui, |ui| {
                ui.label(RichText::new(format!["BINANCE KEYS",]).color(Color32::YELLOW));
                ui.end_row();
                ui.add_sized(
                    egui::vec2(400.0, 20.0),
                    egui::TextEdit::singleline(&mut settings.api_key_enter_string)
                        .hint_text("pub_key"),
                );
                ui.end_row();
                ui.add_sized(
                    egui::vec2(400.0, 20.0),
                    egui::TextEdit::singleline(&mut settings.priv_api_key_enter_string)
                        .hint_text("priv_key"),
                );
                ui.end_row();
                if settings.enc_api_keys == true {
                    ui.add_sized(
                        egui::vec2(400.0, 20.0),
                        egui::TextEdit::singleline(&mut settings.password_string)
                            .hint_text("Encrypt api keys with password: 16 characters or more"),
                    );
                };
                ui.end_row();
                if ui.button("Add keys").clicked() {
                    let msg = ClientInstruct::SendBinInstructs(BinInstructs::AddReplaceApiKeys {
                        pub_key: settings.api_key_enter_string.clone(),
                        priv_key: settings.priv_api_key_enter_string.clone(),
                    });
                    let _res = cli_chan.send(msg);
                    settings.binance_pub_key = Some(settings.api_key_enter_string.clone());
                    settings.binance_priv_key = Some(settings.priv_api_key_enter_string.clone());
                    if settings.enc_api_keys == true {
                        let _res = settings.encrypt_keys(settings.password_string.clone());
                    };
                    //NOTE not sure if this is OK... better way to shred strings
                    settings.api_key_enter_string = String::default();
                    settings.priv_api_key_enter_string = String::default();
                    settings.password_string = String::default();
                };
                if ui.button("Remove keys").clicked() {
                    settings.api_key_enter_string = String::default();
                    settings.priv_api_key_enter_string = String::default();
                    settings.binance_pub_key = None;
                    settings.binance_priv_key = None;
                    settings.enc_binance_pub_key = None;
                    settings.enc_binance_priv_key = None;
                    let msg = ClientInstruct::SendBinInstructs(BinInstructs::RemoveApiKeys {});
                    let _res = cli_chan.send(msg);
                };
                if ui.button("Unlock keys").clicked() {
                    settings.api_key_enter_string = String::default();
                    settings.priv_api_key_enter_string = String::default();
                };
                ui.end_row();
                match settings.key_status {
                    KeysStatus::NotAdded => {
                        ui.label(
                            RichText::new(format!["Add or unlock binance API keys",])
                                .color(Color32::ORANGE),
                        );
                    }
                    KeysStatus::Invalid => {
                        ui.label(RichText::new(format!["Api Keys Invalid",]).color(Color32::RED));
                    }
                    KeysStatus::Valid => {
                        ui.label(RichText::new(format!["Api Keys Valid",]).color(Color32::GREEN));
                    }
                };
            });
        egui::Grid::new("Application settings")
            .striped(true)
            .min_col_width(30.0)
            .show(ui, |ui| {
                ui.label(RichText::new(format!["APPLICATION SETTINGS",]).color(Color32::YELLOW));
                ui.end_row();
                ui.checkbox(
                    &mut settings.save_api_keys,
                    "Store api keys in settings file",
                );
                ui.checkbox(&mut settings.enc_api_keys, "Encrypt api keys w password");
                ui.end_row();
                if ui.button("Save settings").clicked() {
                    if settings.save_api_keys == true {
                        let res = if settings.enc_api_keys == true {
                            settings.binance_pub_key = None;
                            settings.binance_priv_key = None;
                            settings.save_settings_file(Some(settings.password_string.clone()))
                        } else {
                            settings.enc_binance_pub_key = None;
                            settings.enc_binance_priv_key = None;
                            settings.save_settings_file(None)
                        };
                        match res {
                            Ok(_) => (),
                            Err(e) => tracing::error!["Save setting ERROR: {}", e],
                        };
                    } else {
                        settings.binance_pub_key = None;
                        settings.binance_priv_key = None;
                        settings.enc_binance_pub_key = None;
                        settings.enc_binance_priv_key = None;
                        settings.save_settings_file(None);
                    };
                    let mut sett = settings.clone();
                    sett.binance_pub_key = None;
                    sett.binance_priv_key = None;
                    sett.enc_binance_pub_key = None;
                    sett.enc_binance_priv_key = None;
                    let msg = ClientInstruct::UpdateSettings(sett);
                    let _res = cli_chan.send(msg);
                };
            });
        egui::Grid::new("Account_balances")
            .striped(true)
            .min_col_width(30.0)
            .show(ui, |ui| {
                match &settings.key_status {
                    KeysStatus::Valid => {
                        if ui.button("Refresh balances").clicked() {
                            let msg =
                                ClientInstruct::SendBinInstructs(BinInstructs::GetAllBalances {});
                            let _res = cli_chan.send(msg);
                        };
                    }
                    _ => {}
                };
            });
        ui.vertical(|ui| {
            ui.label(RichText::new(format!["BINANCE BLANCES",]).color(Color32::YELLOW));
            let available_height = ui.available_height();
            let mut table = TableBuilder::new(ui)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto().resizable(false))
                .column(Column::auto().resizable(false))
                .column(Column::auto().resizable(false))
                .column(Column::auto().resizable(false))
                .min_scrolled_height(0.0)
                .max_scroll_height(available_height);
            table
                .header(20.0, |mut header| {
                    header.col(|ui| {
                        ui.strong("Asset");
                    });
                    header.col(|ui| {
                        ui.strong("Free balance");
                    });
                    header.col(|ui| {
                        ui.strong("Locked balance");
                    });
                })
                .body(|mut body| {
                    for (asset, (avail_balance, lock_balance)) in settings.balances.iter() {
                        let row_height = 18.0;
                        body.row(row_height, |mut row| {
                            row.col(|ui| {
                                ui.label(format!["{}", asset]);
                            });
                            row.col(|ui| {
                                ui.label(format!["{}", avail_balance]);
                            });
                            row.col(|ui| {
                                ui.label(format!["{}", lock_balance]);
                            });
                        });
                    }
                });
        });
    }
    pub fn encrypt_keys(&mut self, pass: String) -> Result<()> {
        let res = Settings::verify_password_req(&pass);
        match res {
            true => {}
            false => {
                tracing::error!["Password invalid"];
                return Ok(());
            }
        };
        let mc = new_magic_crypt!(&pass, 256);
        let pub_key: String = self
            .binance_pub_key
            .take()
            .ok_or(anyhow!["Pub key string not found!"])?;
        let priv_key: String = self
            .binance_priv_key
            .take()
            .ok_or(anyhow!["Private key string not found!"])?;
        let enc_pub_key = mc.encrypt_str_to_base64(pub_key);
        let enc_priv_key = mc.encrypt_str_to_base64(priv_key);
        self.enc_binance_pub_key = Some(enc_pub_key);
        self.enc_binance_priv_key = Some(enc_priv_key);
        Ok(())
    }
    pub fn decrypt_keys(&mut self, pass: String) -> Result<()> {
        let mc = new_magic_crypt!(&pass, 256);
        let pub_key: String = self
            .enc_binance_pub_key
            .take()
            .ok_or(anyhow!["Pub key string not found!"])?;
        let priv_key: String = self
            .enc_binance_priv_key
            .take()
            .ok_or(anyhow!["Private key string not found!"])?;
        let pub_key = mc.decrypt_base64_to_string(pub_key)?;
        let priv_key = mc.decrypt_base64_to_string(priv_key)?;
        self.binance_pub_key = Some(pub_key);
        self.binance_priv_key = Some(priv_key);
        Ok(())
    }
    pub fn save_settings_file(&mut self, password: Option<String>) -> Result<()> {
        match password {
            Some(pass) => {
                self.encrypt_keys(pass);
            }
            None => {
                if self.save_api_keys == false {
                    self.binance_pub_key = None;
                    self.binance_priv_key = None;
                    self.enc_binance_pub_key = None;
                    self.enc_binance_priv_key = None;
                };
            }
        }
        let config = config::standard();
        let res = bincode::encode_to_vec(self.clone(), config)?;
        let mut file = std::fs::File::create(SETTINGS_SAVE_PATH)?;
        file.write_all(&res)?;
        Ok(())
    }
    pub fn load_settings_enc() -> Result<Option<Self>> {
        let config = config::standard();
        let res = std::fs::File::open(SETTINGS_SAVE_PATH);
        let mut file = match res {
            Ok(file) => file,
            Err(e) => {
                tracing::error!["Load settings error: {}", e];
                return Ok(None);
            }
        };
        let mut data: Vec<u8> = vec![];
        file.read_to_end(&mut data)?;
        let (mut settings, _): (Settings, usize) = bincode::decode_from_slice(&data, config)?;
        Ok(Some(settings))
    }
    pub fn load_settings_file(password: Option<String>) -> Result<Option<Self>> {
        let config = config::standard();
        let res = std::fs::File::open(SETTINGS_SAVE_PATH);
        let mut file = match res {
            Ok(file) => file,
            Err(e) => {
                tracing::error!["Load settings error: {}", e];
                return Ok(None);
            }
        };
        let mut data: Vec<u8> = vec![];
        file.read_to_end(&mut data)?;
        let (mut settings, _): (Settings, usize) = bincode::decode_from_slice(&data, config)?;
        match password {
            Some(pass) => {
                settings.decrypt_keys(pass)?;
            }
            None => {}
        };
        Ok(Some(settings))
    }
}

#[derive(Dbg, Default)]
struct DataManager {
    coin_list: Vec<String>,            //load Assetlist (All binance assets)
    downloaded_coin_list: Vec<String>, //load AssetlistDL (All binance assets)
    search_coin_shortlist: Vec<String>,
    downloaded_coin_shortlist: Vec<String>,

    coin_search_string: String,
    shortlist_max: usize,

    selected_coin: String,

    delete_selected_coin: String,

    max_backdate_months: usize,

    autoupdate_on_start: bool,
    update_success: bool,
    update_ran: bool,
    update_status: String,

    asset_list_loaded: bool,
    asset_list_imported: bool,

    asset_list: Vec<DLAsset>,

    hist_asset_data: Arc<Mutex<AssetData>>,
}

impl DataManager {
    fn new(hist_asset_data: Arc<Mutex<AssetData>>) -> Self {
        DataManager {
            shortlist_max: 10,
            max_backdate_months: 120,
            update_status: "Not ran".to_string(),
            update_ran: false,
            asset_list_loaded: false,
            asset_list_imported: false,
            hist_asset_data,
            ..Default::default()
        }
    }
    fn show(
        data_manager: &mut DataManager,
        cli_chan: watch::Sender<ClientInstruct>,
        ui: &mut egui::Ui,
    ) {
        egui::Grid::new("Data Manager DL")
            .striped(true)
            .min_col_width(30.0)
            .show(ui, |ui| {
                ui.add_sized(
                    egui::vec2(250.0, 20.0),
                    egui::TextEdit::singleline(&mut data_manager.coin_search_string)
                        .hint_text("Add asset to download list for binance"),
                );
                if ui.button("Add").clicked() {
                    let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::InsertDLAsset {
                        symbol: data_manager.coin_search_string.clone(),
                        exchange: "Binance".to_string(),
                    });
                    let _res = cli_chan.send(msg);
                    data_manager.asset_list_loaded = false;
                };
                ui.end_row();
                if ui.button("Update all data").clicked() {
                    let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::UpdateDataAll);
                    let _res = cli_chan.send(msg);
                    data_manager.update_ran = true;
                    data_manager.update_success = true;
                    //TODO connect proper error handling...
                    data_manager.update_status = "Ran".to_string();
                };
            });
        ui.end_row();
        match data_manager.update_success {
            true => {
                ui.label(
                    RichText::new(format![
                        "Autoupdate successful: {}",
                        data_manager.update_status
                    ])
                    .color(Color32::GREEN),
                );
            }
            false => match data_manager.update_ran {
                true => {
                    ui.label(
                        RichText::new(format!["Autoupdate failed: {}", data_manager.update_status])
                            .color(Color32::RED),
                    );
                }
                false => {
                    ui.label(RichText::new(format!["Update not ran"]).color(Color32::ORANGE));
                }
            },
        };
        if data_manager.asset_list_loaded == false {
            let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::LoadDLAssetList);
            let _res = cli_chan.send(msg);
            //problem here is timing... this repeats the signal several times... but it works so
            //wtf...
            let ad = data_manager
                .hist_asset_data
                .lock()
                .expect("Unable to unlock mutex: DATA MANAGER");
            if ad.downloaded_assets.is_empty() == true {
            } else {
                data_manager.asset_list = ad.downloaded_assets.clone();
                data_manager.asset_list_loaded = true;
            }
        };
        if ui.button("Reload asset list").clicked() {
            data_manager.asset_list_loaded = false;
        };
        //NOTE add this but not clickable toggle_ui_compact(ui,&mut data_manager.update_success);
        ui.end_row();

        ui.label(RichText::new(format!["All assets"]).color(Color32::WHITE));
        ui.end_row();
        ui.vertical(|ui| {
            let available_height = ui.available_height();
            let table = TableBuilder::new(ui)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::auto().resizable(false))
                .column(Column::auto().resizable(false))
                .column(Column::auto().resizable(false))
                .column(Column::auto().resizable(false))
                .column(Column::auto().resizable(false))
                .min_scrolled_height(0.0)
                .max_scroll_height(available_height);
            table
                .header(20.0, |mut header| {
                    header.col(|ui| {
                        ui.strong("Asset");
                    });
                    header.col(|ui| {
                        ui.strong("Exchange");
                    });
                    header.col(|ui| {
                        ui.strong("Data Start Time");
                    });
                    header.col(|ui| {
                        ui.strong("Data End Time");
                    });
                })
                .body(|mut body| {
                    for asset in data_manager.asset_list.iter() {
                        if asset.asset != "TEST" {
                            let row_height = 18.0;
                            body.row(row_height, |mut row| {
                                row.col(|ui| {
                                    ui.label(format!["{}", asset.asset]);
                                });
                                row.col(|ui| {
                                    ui.label(format!["{}", asset.exchange]);
                                });
                                row.col(|ui| {
                                    if let Some(start_time) =
                                        chrono::NaiveDateTime::from_timestamp_millis(
                                            asset.dat_start_t,
                                        )
                                    {
                                        ui.label(format!["{}", start_time]);
                                    } else {
                                        ui.label(format!["NaN"]);
                                    };
                                });
                                row.col(|ui| {
                                    if let Some(end_time) =
                                        chrono::NaiveDateTime::from_timestamp_millis(
                                            asset.dat_end_t,
                                        )
                                    {
                                        ui.label(format!["{}", end_time]);
                                    } else {
                                        ui.label(format!["NaN"]);
                                    };
                                });
                                row.col(|ui| {
                                    if ui.button("Delete").clicked() {
                                        let msg = ClientInstruct::SendSQLInstructs(
                                            SQLInstructs::DelAsset {
                                                symbol: asset.asset.clone(),
                                            },
                                        );
                                        let _res = cli_chan.send(msg);
                                        data_manager.asset_list_loaded = false;
                                    }
                                });
                            });
                        };
                    }
                });
        });
    }
}

#[allow(unused)]
enum LineStyle {
    Solid(f32),
    Dotted(f32),
}

#[derive(Copy, Clone, Debug)]
enum LineState {
    ActiveColor(Color32),
    InactiveColor(Color32),
}

#[allow(unused)]
enum HlineType {
    BuyOrder((LineState, LineStyle)),
    SellOrder((LineState, LineStyle)),
    LastPrice((LineStyle, Color32)),
}

const PRICE_STYLE: LineStyle = LineStyle::Solid(1.0);
const LIMIT_STYLE: LineStyle = LineStyle::Solid(1.0);

const DOTT_LINE_SPACING: f32 = 10.0;
const STOP_STYLE: LineStyle = LineStyle::Dotted(1.0);

const BUY_ACTIVE: LineState = LineState::ActiveColor(Color32::GREEN);
const BUY_INACTIVE: LineState = LineState::InactiveColor(Color32::GREEN);

const SELL_ACTIVE: LineState = LineState::ActiveColor(Color32::RED);
const SELL_INACTIVE: LineState = LineState::InactiveColor(Color32::RED);

#[allow(unused)]
const LAST_PRICE_COLOR: Color32 = Color32::YELLOW;
#[allow(unused)]
const LAST_PRICE_LINE: LineStyle = LineStyle::Dotted(0.5);

#[allow(unused)]
impl HlineType {
    fn hline_order(o: &Order, active: bool) -> Vec<HLine> {
        let side = o.get_side();
        let line_state = match (side, active) {
            (true, true) => BUY_ACTIVE,
            (true, false) => BUY_INACTIVE,
            (false, true) => SELL_ACTIVE,
            (false, false) => SELL_INACTIVE,
        };
        match o {
            Order::None => {
                vec![]
            }
            Order::Market { buy, quant } => {
                vec![]
            }
            Order::Limit {
                buy,
                quant,
                price,
                limit_status,
            } => {
                if side == true {
                    vec![
                        HlineType::BuyOrder((line_state, PRICE_STYLE))
                            .to_hline(price, "Limit price"),
                    ]
                } else {
                    vec![
                        HlineType::SellOrder((line_state, PRICE_STYLE))
                            .to_hline(price, "Limit price"),
                    ]
                }
            }
            Order::StopLimit {
                buy,
                quant,
                price,
                limit_status,
                stop_price,
                stop_status,
            } => {
                if side == true {
                    vec![
                        HlineType::BuyOrder((line_state, LIMIT_STYLE))
                            .to_hline(price, "Stop Limit stop"),
                        HlineType::BuyOrder((line_state, STOP_STYLE))
                            .to_hline(&(*stop_price as f64), "Stop Limit stop"),
                    ]
                } else {
                    vec![
                        HlineType::SellOrder((line_state, LIMIT_STYLE))
                            .to_hline(price, "Stop Limit stop"),
                        HlineType::SellOrder((line_state, STOP_STYLE))
                            .to_hline(&(*stop_price as f64), "Stop Limit stop"),
                    ]
                }
            }
            Order::StopMarket {
                buy,
                quant,
                price,
                stop_status,
            } => {
                if side == true {
                    vec![
                        HlineType::BuyOrder((line_state, STOP_STYLE))
                            .to_hline(price, "Stop Market"),
                    ]
                } else {
                    vec![
                        HlineType::SellOrder((line_state, STOP_STYLE))
                            .to_hline(price, "Stop Market"),
                    ]
                }
            }
        }
    }
    fn to_hline(&self, value: &f64, label: &str) -> HLine {
        match &self {
            HlineType::BuyOrder((ba, bs)) => {
                let color = match ba {
                    LineState::ActiveColor(color) => color,
                    LineState::InactiveColor(color) => color,
                };
                match bs {
                    LineStyle::Solid(width) => {
                        let s = Stroke::new(*width, *color);
                        HLine::new(format!["Buy {} {:.2}", label, value], *value)
                            .stroke(s)
                            .style(LineStyleEgui::Solid)
                    }
                    LineStyle::Dotted(width) => {
                        let s = Stroke::new(*width, *color);
                        HLine::new(format!["Buy {} {:.2}", label, value], *value)
                            .stroke(s)
                            .style(LineStyleEgui::Dotted {
                                spacing: DOTT_LINE_SPACING,
                            })
                    }
                }
            }
            HlineType::SellOrder((si, ss)) => {
                let color = match si {
                    LineState::ActiveColor(color) => color,
                    LineState::InactiveColor(color) => color,
                };
                match ss {
                    LineStyle::Solid(width) => {
                        let s = Stroke::new(*width, *color);
                        HLine::new(format!["Sell {} {:.2}", label, value], *value)
                            .stroke(s)
                            .style(LineStyleEgui::Solid)
                    }
                    LineStyle::Dotted(width) => {
                        let s = Stroke::new(*width, *color);
                        HLine::new(format!["Sell {} {:.2}", label, value], *value)
                            .stroke(s)
                            .style(LineStyleEgui::Dotted {
                                spacing: DOTT_LINE_SPACING,
                            })
                    }
                }
            }
            HlineType::LastPrice((l, color)) => match l {
                LineStyle::Solid(width) => {
                    let s = Stroke::new(*width, *color);
                    HLine::new("Last price", *value)
                        .stroke(s)
                        .style(LineStyleEgui::Solid)
                }
                LineStyle::Dotted(width) => {
                    let s = Stroke::new(*width, *color);
                    HLine::new("Last price", *value)
                        .stroke(s)
                        .style(LineStyleEgui::Dotted {
                            spacing: DOTT_LINE_SPACING,
                        })
                }
            },
        }
    }
}

impl HistPlot {
    fn new(hist_asset_data: Arc<Mutex<AssetData>>, intv: &Intv, default_trade_wicks: i64) -> Self {
        Self {
            intv: *intv,
            last_intv: *intv,

            trade_wicks: default_trade_wicks as u16,
            navi_wicks_s: format!["{}", default_trade_wicks],
            hist_asset_data,
            ..Default::default()
        }
    }
    fn show(
        hist_plot: &mut HistPlot,
        plot_extras: &PlotExtras,
        cli_chan: watch::Sender<ClientInstruct>,
        hist_ad: Arc<Mutex<AssetData>>,
        ui: &mut egui::Ui,
        trade_slice: Option<&mut Vec<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>>,
        hist_extras: &mut HistExtras,
    ) {
        if hist_plot.trade_slice_loaded == true {
            let res = hist_plot.kline_plot.show_live(
                ui,
                plot_extras,
                hist_ad,
                None,
                None,
                Some(hist_plot.trade_wicks as usize),
                None,
                Some(&mut hist_extras.symbol_info),
            );
            match (res, trade_slice) {
                (Some(ret_slice), Some(t_slice)) => {
                    hist_extras.last_price = ret_slice[ret_slice.len() - 1].4;
                    tracing::debug!["hist_extras.last_price {}", hist_extras.last_price];
                    *t_slice = ret_slice;
                }
                _ => {}
            };
        } else {
            let _res = hist_plot.kline_plot.show_live(
                ui,
                plot_extras,
                hist_ad,
                None,
                None,
                None,
                Some(&mut hist_extras.last_price),
                Some(&mut hist_extras.symbol_info),
            );
        };

        if hist_plot.intv != hist_plot.last_intv {
            hist_plot.last_intv = hist_plot.intv;
            hist_plot.kline_plot.intv = hist_plot.intv;
        };

        ui.end_row();
        egui::Grid::new("Hplot order assets:")
            .striped(true)
            .show(ui, |ui| {
                egui::ComboBox::from_label("")
                    .selected_text(format!("{}", hist_plot.intv.to_str()))
                    .show_ui(ui, |ui| {
                        for i in Intv::iter() {
                            ui.selectable_value(&mut hist_plot.intv, i, i.to_str());
                        }
                    });
                ui.add_sized(
                    egui::vec2(100.0, 20.0),
                    egui::TextEdit::singleline(&mut hist_plot.search_load_string)
                        .hint_text("Search for asset"),
                );
                ui.add(
                    egui_extras::DatePickerButton::new(&mut hist_plot.picked_date_end)
                        .id_salt("trade_time"),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut hist_plot.trade_h_s).hint_text("Trade hours"),
                );
                /*
                ui.add_sized(
                    egui::vec2(0.5, 20.0),
                    egui::Label::new(":"),
                );
                */
                ui.add(
                    egui::TextEdit::singleline(&mut hist_plot.trade_min_s).hint_text("Trade mins"),
                );
                if ui.button("Go to").clicked() {
                    let res: Result<u16, ParseIntError> = hist_plot.trade_h_s.parse();
                    let trade_h = match res {
                        Ok(n) => {
                            if n <= 23 {
                                n
                            } else {
                                tracing::error!["Unable to parse hour: larger than 23"];
                                hist_plot.trade_h_s = "00".to_string();
                                0
                            }
                        }
                        Err(e) => {
                            tracing::error!["Parsing error for hour: {}", e];
                            0
                        }
                    };
                    let res: Result<u16, ParseIntError> = hist_plot.trade_min_s.parse();
                    let trade_min = match res {
                        Ok(n) => {
                            if n <= 59 {
                                n
                            } else {
                                tracing::error!["Unable to parse minutes: larger than 59"];
                                hist_plot.trade_min_s = "00".to_string();
                                0
                            }
                        }
                        Err(e) => {
                            tracing::error!["Parsing error for hour: {}", e];
                            0
                        }
                    };
                    let trade_date = hist_plot.picked_date_end.clone();
                    let trade_time = trade_date
                        .and_hms(trade_h.into(), trade_min.into(), 0)
                        .timestamp_millis();
                    tracing::trace!["GO to>> END: {}", trade_time];
                    let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::LoadHistDataPart2 {
                        symbol: hist_plot.search_load_string.clone(),
                        trade_time: trade_time,
                        backload_wicks: BACKLOAD_WICKS,
                    });
                    hist_plot.kline_plot.symbol = hist_plot.search_load_string.clone();
                    hist_plot.trade_time = trade_time;
                    let _res = cli_chan.send(msg);
                };
                //ui.end_row();
                if ui.button("Trade N wicks >>").clicked() {
                    let res: Result<u16, ParseIntError> = hist_plot.trade_wicks_s.parse();
                    let n_wicks = match res {
                        Ok(n) => n,
                        Err(e) => {
                            tracing::error!["Parsing error for trade wicks: {}", e];
                            hist_plot.trade_wicks_s = format!["{}", DEFAULT_TRADE_WICKS];
                            DEFAULT_TRADE_WICKS
                        }
                    };

                    hist_plot.navi_wicks = n_wicks;

                    let new_trade_time =
                        hist_plot.trade_time + hist_plot.intv.to_ms() * (n_wicks as i64);
                    tracing::trace![
                        "Trade >> START: {} END: {}",
                        &hist_plot.trade_time,
                        &new_trade_time
                    ];
                    let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::LoadHistDataPart2 {
                        symbol: hist_plot.search_load_string.clone(),
                        trade_time: new_trade_time,
                        backload_wicks: BACKLOAD_WICKS,
                    });
                    let _res = cli_chan.send(msg);
                    hist_plot.trade_time = new_trade_time;
                    hist_plot.trade_slice_loaded = true;
                    //FIXME click here
                    //
                    //
                    //
                    //
                }
                ui.add(
                    egui::TextEdit::singleline(&mut hist_plot.trade_wicks_s)
                        .hint_text("Trade N wicks"),
                );
                ui.end_row();
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample() {
        assert!(true);
    }
}
