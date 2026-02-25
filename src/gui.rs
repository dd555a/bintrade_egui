use std::fmt;
use std::ops::Range;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use log::debug;

use strum::IntoEnumIterator;
use strum_macros::EnumIter;

use anyhow::{Context, Result, anyhow};
use std::result::Result as OCResult;

use tokio::sync::oneshot;
use tokio::sync::watch;

use eframe::egui::{self, DragValue, Event, Vec2};

use crate::{ClientInstruct, ClientResponse, ProcResp, SQLInstructs, SQLResponse};

use crate::binance::KlineTick;

use egui::{Color32, ComboBox, epaint};
use egui_plot_bintrade::{
    AxisHints, Bar, BarChart, BoxElem, BoxPlot, BoxSpread, GridInput, GridMark, HLine, HPlacement,
    Legend, Line, Plot, PlotPoints, PlotUi, uniform_grid_spacer,
};
use egui_tiles::{Tile, TileId, Tiles};
use epaint::Stroke;

use crate::data::{AssetData, Intv};

use crate::trade::{HistTrade, LimitStatus, Order, Quant, StopStatus};
use std::collections::BTreeMap;

use crate::binance::SymbolOutput;

#[derive(PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
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
        }
    }
}

const WICKS_VISIBLE: usize = 90;
const NAVI_WICKS_DEFAULT: u16 = 30;
const CHART_FORWARD: u16 = 40;
const VIEW_WINDOW: usize = 3_000;

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
    ) -> Result<()> {
        let ad = live_ad.lock().expect("Live AD mutex locked");

        let chart_ad_id = ad.id.clone();

        let symbol = self.symbol.clone();
        if let Some(col_data) = collected_data {
            if let Some(data) = col_data.get(&symbol) {
                tracing::trace!["Collected data non empty"];
                //tracing::debug!["Show live SOME for {}, chart_ad_id:{}", &symbol, &chart_ad_id];
                self.live_live_from_ad(
                    &ad,
                    &symbol,
                    self.intv.clone(),
                    self.intv.to_view_window(),
                    None,
                    &data,
                );
            };
            self.live_from_ad(
                &ad,
                &symbol,
                self.intv.clone(),
                self.intv.to_view_window(),
                None,
                false,
            );
        } else {
            self.live_from_ad(
                &ad,
                &symbol,
                self.intv.clone(),
                self.intv.to_view_window(),
                None,
                true,
            );
        };
        self.show(ui, plot_extras);

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
                self.offset -= (n_wicks as i64);
            };
            let search = ui
                .add(egui::TextEdit::singleline(&mut self.navi_wicks_s).hint_text("Navi N wicks"));
            if ui.button("Navi >>").clicked() {
                let res: Result<u16, ParseIntError> = self.navi_wicks_s.parse();
                let n_wicks = match res {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!["Parsing error for navigation wicks: {}", e];
                        NAVI_WICKS_DEFAULT
                    }
                };
                self.offset += (n_wicks as i64);
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
                self.y_offset += (n_wicks as i64);
            }
            let search = ui.add(
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
                self.y_offset -= (n_wicks as i64);
            }
            if ui.button("Reset offset y").clicked() {
                self.y_offset = 0;
            }
        });
        /*
         * */
        Ok(())
    }
    fn mk_plt(&self) -> (Plot, Plot) {
        let (x_lower, x_higher) = self.x_bounds;
        let (y_lower, y_higher) = self.y_bounds;
        let v_higher = self.v_bound;
        make_plot(
            &self.name, self.intv, y_lower, y_higher, x_lower, x_higher, v_higher,
        )
    }
    fn add_hist(&mut self, klines: &Klines) {
        let res = klines.dat.get(&self.intv);
        match res {
            Some(kline) => {
                let (divider, width) = get_chart_params(&self.intv);
                self.add_live(&kline.kline, &divider, &width, false)
            }
            None => {
                tracing::info!["Kline empty for intv:{}", &self.intv.to_str()]
            }
        }
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
            self.x_bounds_set == true;
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
    fn add_live_single(
        &mut self,
        slice: &(chrono::NaiveDateTime, f64, f64, f64, f64, f64),
        divider: &f64,
        width: &f64,
    ) {
        let (x, _, h, _, _, _) = slice;
        let (_, highest) = self.y_bounds;
        if h > &highest {
            self.y_bounds = (0.0, h.clone());
        };
        let (x_old, _) = self.x_bounds;
        let x_new = x.timestamp() as f64;
        self.x_bounds = (x_old, x_new);
        let (boxe, bar) = box_element(slice, divider, width);
        self.l_boxplot.push(boxe);
        self.l_barchart.push(bar);
    }
    fn reset_chart(&mut self) -> Result<()> {
        Ok(())
    }
    fn show(&mut self, ui: &mut egui::Ui, plot_extras: &PlotExtras) -> Result<()> {
        let (plot_candles, plot_volume) = self.mk_plt();
        //Handle live tick as separate box...
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
                    log::trace!["Hlines in plot: {:?}", &hlines];
                    if hlines != [] {
                        for h in hlines {
                            log::trace!["Hlines (for h in ){:?}", &h];
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
    fn live_from_sql() {}
    fn live_live_from_ad(
        &mut self,
        ad: &AssetData,
        symbol: &str,
        intv: Intv,
        max_load_points: usize,
        timestamps: Option<(chrono::NaiveDateTime, chrono::NaiveDateTime)>,
        data: &SymbolOutput,
    ) -> Result<()> {
        //tracing::debug!["\x1b[93m  live_live ran \x1b[0m"];
        //initial_klines

        let k = if let Some(ck) = data.closed_klines.get(&intv) {
            KlineTick::to_kline_vec(ck)
        } else {
            vec![]
        };
        let ok = if let Some(ok) = data.all_klines.get(&intv) {
            KlineTick::to_kline_vec(ok)
        } else {
            vec![]
        };
        let (div, width) = get_chart_params(&intv);
        self.chart_params = (div, width);
        if k.len() <= max_load_points {
            if k.is_empty() == false {
                tracing::trace!["Live KLINES added 1"];
                self.add_live(&k, &div, &width, true);
                tracing::trace!["Live KLINES added {:?}", self.l_boxplot];
            };
        } else {
            let kl = &k[(k.len() - max_load_points)..];
            if kl.is_empty() == false {
                self.add_live(&kl, &div, &width, true);
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
    fn from_watch_channel(
        &mut self,
        mut kline_chan: watch::Receiver<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>,
    ) {
        let kl = kline_chan.borrow_and_update().clone();
        let (div, width) = self.chart_params;
        self.add_live_single(&kl, &div, &width);
    }
    fn tick_from_watch_channel(
        &mut self,
        mut kline_chan: watch::Receiver<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>,
    ) {
        let kl = kline_chan.borrow_and_update().clone();
        self.tick_kline = Some(kl);
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

fn kline_boxes(
    kline_input: &[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)],
    divider: &f64,
    width: &f64,
) -> (Vec<BoxElem>, Vec<Bar>) {
    let n = kline_input.len();
    let mut out_kline: Vec<BoxElem> = vec![];
    let mut out_volume: Vec<Bar> = vec![];
    for line in kline_input.iter() {
        let (kline, volume) = box_element(line, &divider, &width);
        out_kline.push(kline);
        out_volume.push(volume);
    }
    return (out_kline, out_volume);
}

macro_rules! make_p{
    ( $($name:ident, $formatter:ident, $formatter2:ident, $y_lower:ident, $y_upper:ident, $x_lower:ident, $x_higher:ident),* ) => {
        {
            let plot = Plot::new($($name.to_string())*)
                .legend(Legend::default())
                .x_axis_formatter($($formatter)*)
                .set_margin_fraction([0.1,0.1].into())
                .default_y_bounds($($y_lower)*,$($y_upper)*)
                .default_x_bounds($($x_lower)*,$($x_higher)*)
                .height(320.0)
                .view_aspect(2.0);
                //.x_grid_spacer(uniform_grid_spacer($($formatter2)*))
                //TODO BUG if this is not here... the gui doesn't crash...
                //.clamp_grid(true)
                //add bounds with ability to plot
                //Make volume a separate chart?
                //.auto_bounds([true,true])

            plot
        }
    };
}
macro_rules! make_p2{
    ( $($name:ident, $formatter:ident, $formatter2:ident, $y_lower:ident, $y_upper:ident, $x_lower:ident, $x_higher:ident, $v_higher:ident),* ) => {
        {
            let id=format!["{}",format!["plot_id_{}",$($name.to_string())*]];
            let mut axis_hints=AxisHints::new_y();
            axis_hints.clone().placement(HPlacement::Right);
            //TODO - to togle percentage change the x axis formatter
            //TODO - find a way to place the chart labels on the right... the above obviously
            //TODO - add a tick wick and a tick line...
            //doesn't work...
            let candle_plot = Plot::new($($name.to_string())*)
                .legend(Legend::default())
                .link_cursor(id.clone(), [true,false])
                .link_axis(id.clone(), [true,false])
                .width(560.0)
                .height(250.0)
                .custom_x_axes(vec![])
                .custom_y_axes(vec![])
                .x_axis_formatter($($formatter)*)
                //.set_margin_fraction([0.1,0.1].into())
                .default_y_bounds($($y_lower)*,$($y_upper)*)
                .default_x_bounds($($x_lower)*,$($x_higher)*);
            let volume_plot = Plot::new(format!["{}_volume",$($name.to_string())*])
                .legend(Legend::default())
                .link_cursor(id.clone(), [true,false])
                .link_axis(id.clone(), [true,false])
                .width(560.0)
                .height(80.0)
                //.set_margin_fraction([0.1,0.1].into())
                //.default_x_bounds($($x_lower)*,$($x_higher)*)
                .default_y_bounds(0.0,$($v_higher)*)
                .custom_y_axes(vec![])
                .x_axis_formatter($($formatter)*);
                //.default_x_bounds($($x_lower)*,$($x_higher)*);

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
) -> (Plot, Plot) {
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

//NOTE DAY1
//    let width=0.8*4.0;
//    let divider=60.0*4.0*4.0*6.0*4.0;
//NOTE 6H
//    let width=0.8*4.0;
//    let divider=60.0*4.0*4.0*6.0;
//NOTE 1H
//    let width=0.8*4;
//    let divider=60.0*4.0*4.0;
//NOTE 15min
//    let width=0.8*4;
//    let divider=60.0*4.0;
//NOTE 1min
//    let width=0.8;
//    let divider=60.0;

const m1_div: i64 = 60;
const m3_div: i64 = m1_div * 3;
const m5_div: i64 = m1_div * 5;
const m15_div: i64 = m1_div * 15;
const m30_div: i64 = m1_div * 30;
const h1_div: i64 = m1_div * 60;
const h2_div: i64 = h1_div * 2;
const h4_div: i64 = h1_div * 4;
const h6_div: i64 = h1_div * 6;
const h8_div: i64 = h1_div * 8;
const h12_div: i64 = h1_div * 12;
const d1_div: i64 = h1_div * 24;
const d3_div: i64 = d1_div * 3;
const w1_div: i64 = d1_div * 7;
const mo1_div: i64 = d1_div * 30; //30 is kind of a hack... but it works... so wtf...

const gap: f64 = 45.0;
const extra_gap: f64 = 1.2;

const m1_gap: f64 = gap / (m1_div as f64);
const m3_gap: f64 = (gap * 3.0) / (m3_div as f64);
const m5_gap: f64 = (gap * 5.0) / (m5_div as f64);
const m15_gap: f64 = (gap * 15.0) / (m15_div as f64);
const m30_gap: f64 = (gap * 30.0) / (m30_div as f64);
const h1_gap: f64 = (gap * 60.0) / (h1_div as f64);
const h2_gap: f64 = (gap * 60.0 * 2.0) / (h2_div as f64);
const h4_gap: f64 = (gap * 60.0 * 4.0) / (h4_div as f64);
const h6_gap: f64 = (gap * 60.0 * 6.0) / (h6_div as f64);
const h8_gap: f64 = (gap * 60.0 * 8.0) / (h8_div as f64);
const h12_gap: f64 = (gap * 60.0 * 12.0) / (h12_div as f64);
const d1_gap: f64 = (gap * 60.0 * 24.0) / (d1_div as f64);
const d3_gap: f64 = (extra_gap * gap * 60.0 * 72.0) / (d3_div as f64);
const w1_gap: f64 = (extra_gap * gap * 60.0 * 24.0 * 7.0) / (w1_div as f64);
const mo1_gap: f64 = (extra_gap * gap * 60.0 * 24.0 * 30.0) / (mo1_div as f64);

fn get_chart_params(intv: &Intv) -> (f64, f64) {
    match intv {
        Intv::Min1 => (m1_div as f64, m1_gap), //incorrect
        Intv::Min3 => (m3_div as f64, m3_gap),
        Intv::Min5 => (m5_div as f64, m5_gap),
        Intv::Min15 => (m15_div as f64, m15_gap),
        Intv::Min30 => (m30_div as f64, m30_gap),
        Intv::Hour1 => (h1_div as f64, h1_gap),
        Intv::Hour2 => (h2_div as f64, h2_gap),
        Intv::Hour4 => (h4_div as f64, h4_gap),
        Intv::Hour6 => (h6_div as f64, h6_gap),
        Intv::Hour8 => (h8_div as f64, h8_gap),
        Intv::Hour12 => (h12_div as f64, h12_gap),
        Intv::Day1 => (d1_div as f64, d1_gap),
        Intv::Day3 => (d3_div as f64, d3_gap),
        Intv::Week1 => (w1_div as f64, w1_gap),
        Intv::Month1 => (mo1_div as f64, mo1_gap), //with reference to 1970 1,1 00:00 perhaps?
    }
}
//TODO convert to macros or fork egui_plot library and make a better implementation yourself...
fn grid_spacer_1min(input: GridInput) -> [f64; 3] {
    [60.0, 60.0, 1.0]
}

fn x_format_1min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * m1_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_3min(input: GridInput) -> [f64; 3] {
    [60.0, 60.0, 1.0]
}

fn x_format_3min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * m3_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_5min(input: GridInput) -> [f64; 3] {
    [60.0, 60.0, 1.0]
}

fn x_format_5min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * m5_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_15min(input: GridInput) -> [f64; 3] {
    [15.0, 15.0, 1.0]
}

fn x_format_15min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * m15_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_30min(input: GridInput) -> [f64; 3] {
    [15.0, 15.0, 1.0]
}

fn x_format_30min(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * m30_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_1h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

fn x_format_1h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * h1_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_2h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}
fn x_format_2h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * h2_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_4h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

fn x_format_4h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * h4_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}

fn grid_spacer_6h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

fn x_format_6h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * h6_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_8h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

fn x_format_8h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * h8_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_12h(input: GridInput) -> [f64; 3] {
    [4.0, 4.0, 1.0]
}

fn x_format_12h(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * h12_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_1d(input: GridInput) -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

fn x_format_1d(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * d1_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_3d(input: GridInput) -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

fn x_format_3d(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * d3_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_1w(input: GridInput) -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

fn x_format_1w(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * w1_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}
fn grid_spacer_1mo(input: GridInput) -> [f64; 3] {
    [1.0, 1.0, 1.0]
}

fn x_format_1mo(gridmark: GridMark, range: &RangeInclusive<f64>) -> String {
    let fixed_gridmark = (gridmark.value as i64) * mo1_div;
    let date_time = chrono::NaiveDateTime::from_timestamp(fixed_gridmark, 0);
    format!["{}", date_time]
}

fn time_format(input: &BoxElem, plot: &BoxPlot) -> String {
    let red = Color32::from_rgb(255, 0, 0);
    let green = Color32::from_rgb(0, 255, 0);
    let (o, c) = match input.fill {
        red => (input.spread.quartile1, input.spread.quartile3),
        green => (input.spread.quartile3, input.spread.quartile1),
    };
    format!(
        "Time: {} \n Open: {} \n High: {} \n Low: {} \n Close: {} \n Average: {}",
        input.name,
        o,
        input.spread.upper_whisker,
        input.spread.lower_whisker,
        c,
        input.spread.median
    )
}

//NOTE interval formatter 2 values = width (0.8 for 1min), divider (60 for 1 min), spacer(60 60 1
//for 1min)

#[derive(EnumIter, Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]

enum PaneType {
    None,
    LiveTrade,
    HistTrade,
    ManageData,
    Account,
    Settings,
}
impl fmt::Display for PaneType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PaneType::LiveTrade => write!(f, "Live Trade"),
            PaneType::HistTrade => write!(f, "Hist Trade"),
            PaneType::ManageData => write!(f, "Manage Data"),
            PaneType::Account => write!(f, "Account"),
            PaneType::Settings => write!(f, "Settings"),
            PaneType::None => write!(f, "None"),
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
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
        let mut response: egui_tiles::UiResponse;
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
            PaneType::None => {
                test_chart2(ui);
            }
            PaneType::LiveTrade => {
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");
                let live_price = self.live_price.lock().expect("Live price mutex poisoned!");
                let live_plot_l = self.live_plot.clone();
                let c_data = self
                    .collect_data
                    .lock()
                    .expect("Live collect data mutex poisoned!");

                let p_extras: PlotExtras = PlotExtras::None;
                let mut live_plot = live_plot_l.lock().expect("Live plot mutex posoned!");
                //NOTE... see if chan works too... should...
                LivePlot::show(&mut live_plot, &p_extras, chan, &live_price, &c_data, ui);
                let mut man_orders = self
                    .man_orders
                    .get_mut(&pane.nr)
                    .expect("Manual order window not found!");
                if man_orders.plot_extras.is_some() != true {
                    man_orders.plot_extras = Some(p_extras);
                };
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");
                ManualOrders::show(
                    &mut man_orders,
                    &live_price,
                    None,
                    chan,
                    ui,
                    Some(&mut live_plot.kline_plot.hlines),
                    None,
                );
            }
            PaneType::HistTrade => {
                let sql_resps: Option<&Vec<ClientResponse>> = match self.resp_buff.as_ref() {
                    Some(a) => a.get(&ProcResp::SQLResp(SQLResponse::None)),
                    None => None,
                };
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");
                let hist_extras = HistExtras::default();
                let p_extras: PlotExtras = PlotExtras::None;
                let mut h_plot = self
                    .hist_plot
                    .get_mut(&pane.nr)
                    .expect("Hist plot gui struct not found!");
                if h_plot.hist_extras.is_some() != true {
                    h_plot.hist_extras = Some(hist_extras);
                };

                //let hist_trading=&mut h_plot.hist_trade;

                HistPlot::show(
                    &mut h_plot,
                    &p_extras,
                    chan,
                    self.hist_asset_data.clone(),
                    ui,
                );

                let mut man_orders = self
                    .man_orders
                    .get_mut(&pane.nr)
                    .expect("Manual order window not found!");
                if man_orders.plot_extras.is_some() != true {
                    man_orders.plot_extras = Some(p_extras);
                };
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");
                ManualOrders::show(
                    &mut man_orders,
                    &hist_extras.last_price,
                    Some(&mut h_plot.hist_trade),
                    chan,
                    ui,
                    Some(&mut h_plot.kline_plot.hlines),
                    None,
                );
            }
            PaneType::ManageData => {
                let sql_resps: Option<&Vec<ClientResponse>> = match self.resp_buff.as_ref() {
                    Some(a) => a.get(&ProcResp::SQLResp(SQLResponse::None)),
                    None => None,
                };
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");

                let data_manager_l = self.data_manager.clone();
                let mut data_manager = data_manager_l.lock().expect("Data manager mutex posoned!");

                DataManager::show(&mut data_manager, chan, ui);
            }
            PaneType::Account => {
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");

                let account_l = self.account.clone();
                let mut account = account_l.lock().expect("Data manager mutex posoned!");

                Account::show(&mut account, chan, ui);
            }
            PaneType::Settings => {
                let chan = self.send_to_cli.clone().expect("Cli comm channel none!");

                let settings_l = self.settings.clone();
                let mut settings = settings_l.lock().expect("Data manager mutex posoned!");

                Settings::show(&mut settings, chan, ui);
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
                        }
                        PaneType::ManageData => {}
                        PaneType::Account => {}
                        PaneType::Settings => {}
                    }
                    let tab_title = self.tab_title_for_pane(pane);
                    log::debug!("Closing tab: {}, tile ID: {tile_id:?}", tab_title.text());
                }
                Tile::Container(container) => {
                    log::debug!("Closing container: {:?}", container.kind());
                    let children_ids = container.children();
                    for child_id in children_ids {
                        if let Some(Tile::Pane(pane)) = tiles.get(*child_id) {
                            let tab_title = self.tab_title_for_pane(pane);
                            log::debug!("Closing tab: {}, tile ID: {tile_id:?}", tab_title.text());
                        }
                    }
                }
            }
        }
        true
    }
}

fn create_tree() -> egui_tiles::Tree<Pane> {
    let mut gen_pane = || {
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

#[derive(PartialEq, Debug, Clone)]
enum PlotExtras {
    None,
    OrderHlines(Vec<HLine>),
}

#[derive(Dbg)]
struct DataAsset {}

use derive_debug::Dbg;

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
pub struct DesktopApp {
    simplification_options: egui_tiles::SimplificationOptions,
    tab_bar_height: f32,
    gap_width: f32,
    add_child_to: Option<egui_tiles::TileId>,

    pane_number: usize,
    #[dbg(skip)]
    tree: Rc<Mutex<egui_tiles::Tree<Pane>>>,

    #[cfg_attr(feature = "serde", serde(skip))]
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
    account: Rc<Mutex<Account>>,
    settings: Rc<Mutex<Settings>>,

    lp_chan_recv: watch::Receiver<f64>,

    send_to_cli: Option<watch::Sender<ClientInstruct>>,
    recv_from_cli: Option<watch::Receiver<ClientResponse>>,

    resp_buff: Option<HashMap<ProcResp, Vec<ClientResponse>>>,
    last_resp: Option<ClientResponse>,

    man_orders: BTreeMap<usize, ManualOrders>,
    hist_plot: BTreeMap<usize, HistPlot>,
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
        // using :
        let hat = Rc::new(Mutex::new(HistPlot::default()));
        let lp = Arc::new(Mutex::new(0.0));
        let asset_data = Arc::new(Mutex::new(AssetData::new(6661)));
        let hist_asset_data = Arc::new(Mutex::new(AssetData::new(6662)));
        let cd = Arc::new(Mutex::new(HashMap::new()));

        let (_, lp_chan_recv) = watch::channel(0.0);

        let man_o = ManualOrders::default();
        let mut man_o_default = BTreeMap::new();
        man_o_default.insert(0, man_o);

        Self {
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

            account: Rc::new(Mutex::new(Account::new())),
            settings: Rc::new(Mutex::new(Settings::new())),

            //Channels
            send_to_cli: None,
            recv_from_cli: None,

            man_orders: man_o_default,
            hist_plot: BTreeMap::new(),
        }
    }
}

impl DesktopApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        schan: watch::Sender<ClientInstruct>,
        rchan: watch::Receiver<ClientResponse>,
        asset_data: Arc<Mutex<AssetData>>,
        hist_asset_data: Arc<Mutex<AssetData>>,
        live_price: Arc<Mutex<f64>>,
        collect_data: Arc<Mutex<HashMap<String, SymbolOutput>>>,
    ) -> Self {
        cc.storage;
        let live_plot = Rc::new(Mutex::new(LivePlot::new(asset_data.clone())));
        let data_manager = Rc::new(Mutex::new(DataManager::new(hist_asset_data.clone())));
        Self {
            send_to_cli: Some(schan),
            recv_from_cli: Some(rchan),
            live_price,
            live_plot,
            collect_data,
            asset_data,
            hist_asset_data,
            data_manager,
            ..Default::default()
        }
    }
    fn pane_id() {}
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
                        let chan = match self.send_to_cli.clone() {
                            Some(chan) => {
                                let msg = ClientInstruct::Terminate;
                                chan.send(msg);
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
                                    .insert(self.pane_number + 1, ManualOrders::default());
                                let ad = self.hist_asset_data.lock().expect("Debug mutex");
                                //tracing::debug!["HIST TRADE CREATION {}",ad.debug()];
                                self.hist_plot.insert(
                                    self.pane_number + 1,
                                    HistPlot::new(Arc::clone(&self.hist_asset_data)),
                                );

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
                            PaneType::Account => {
                                new_child = tree.tiles.insert_pane(Pane::new(
                                    self.pane_number + 1,
                                    PaneType::Account,
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

            //if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
            /*
            if let Some(pos) = ctx.input_mut(|i| {
                let mods=egui::Modifiers { ctrl: true, alt: true, ..Default::default() };
                let key=egui::Key::A;
                let shortcut=egui::KeyboardShortcut::new(mods,key);
                let bool_state=i.consume_key(&shortcut);
            });
            */

            if ctx.input(|i| i.key_pressed(egui::Key::A)) {
                println!("\n A Pressed");
            }
            if ctx.input(|i| i.key_down(egui::Key::A)) {
                println!("\n A Held");
                ui.ctx().request_repaint(); // make sure we note the holding.
            }
            if ctx.input(|i| i.key_released(egui::Key::A)) {
                println!("\n A Released");
            }
            let tt = self.tree.clone();
            let mut tree = tt.lock().expect("Posoned mutex on pane tree!");
            tree.ui(self, ui);
        });
    }
}

pub fn password_ui(ui: &mut egui::Ui, password: &mut String) -> egui::Response {
    // This widget has its own state — show or hide password characters (`show_plaintext`).
    // In this case we use a simple `bool`, but you can also declare your own type.
    // It must implement at least `Clone` and be `'static`.
    // If you use the `persistence` feature, it also must implement `serde::{Deserialize, Serialize}`.

    // Generate an id for the state
    let state_id = ui.id().with("show_plaintext");

    // Get state for this widget.
    // You should get state by value, not by reference to avoid borrowing of [`Memory`].
    let mut show_plaintext = ui.data_mut(|d| d.get_temp::<bool>(state_id).unwrap_or(false));

    // Process ui, change a local copy of the state
    // We want TextEdit to fill entire space, and have button after that, so in that case we can
    // change direction to right_to_left.
    let result = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // Toggle the `show_plaintext` bool with a button:
        let response = ui
            .selectable_label(show_plaintext, "👁")
            .on_hover_text("Show/hide password");

        if response.clicked() {
            show_plaintext = !show_plaintext;
        }

        // Show the password field:
        ui.add_sized(
            ui.available_size(),
            egui::TextEdit::singleline(password).password(!show_plaintext),
        );
    });

    // Store the (possibly changed) state:
    ui.data_mut(|d| d.insert_temp(state_id, show_plaintext));

    // All done! Return the interaction response so the user can check what happened
    // (hovered, clicked, …) and maybe show a tooltip:
    result.response
}

//ui.add(password(&mut my_password));
pub fn password(password: &mut String) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| password_ui(ui, password)
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Keep,
    Delete,
}

use crate::trade::HistTrade as HistTradeRunner;
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
struct ManualOrders {
    man_orders: Option<HistTrade>,
    active_orders: Option<Vec<Order>>,

    hist_trade_runner: HistTradeRunner,

    current_symbol: String,

    search_string: String,
    quant: Quant,
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

    last_price_buffer: Vec<f64>,
    last_price_buffer_size: usize,
    last_price_s: f64,

    plot_extras: Option<PlotExtras>,
}

use std::collections::HashMap;

//TODO find a way to group GUI elements together. The horror...
impl Default for ManualOrders {
    fn default() -> Self {
        Self {
            man_orders: None,
            active_orders: None,

            current_symbol: "BTCUSDT".to_string(),

            search_string: "".to_string(),
            quant: Quant::Q100,
            order: Order::Market {
                buy: true,
                quant: Quant::Q100,
            },
            new_order: Order::Market {
                buy: true,
                quant: Quant::Q100,
            },
            scalar: 0.0,
            scalar_set: false,
            buy: true,

            hist_trade_runner: HistTradeRunner::default(),

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

            plot_extras: None,
        }
    }
}
use egui::{FontId, RichText};

#[instrument(level = "trace")]
fn link_hline_orders(orders: &HashMap<i32, (Order, bool)>, hlines: &mut Vec<HLine>) {
    let mut hl: Vec<HLine> = orders
        .iter()
        .map(|(_, (order, active))| HlineType::hline_order(order, *active))
        .collect();
    tracing::trace!["Hlines (link_hline_orders):{:?}", &hl];
    hlines.clear();
    hlines.append(&mut hl);
}

impl ManualOrders {
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
        hist_trading: Option<&mut HistTradeRunner>,
    ) -> Result<()> {
        match hlines {
            Some(hline_ref) => link_hline_orders(&man_orders.orders, hline_ref),
            None => {
                tracing::debug!["Hlines not available!"];
            }
        };
        //man_orders.sync(hist_trade.as_deref());
        //man_orders.sync(hist_trade.as_deref());
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
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(format!["{}:{}", man_orders.asset1_name, man_orders.asset1])
                            .color(Color32::GREEN),
                    );
                    ui.end_row();
                });
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(format!["{}:{}", man_orders.asset2_name, man_orders.asset2])
                            .color(Color32::RED),
                    );
                    ui.end_row();
                });
            });
        ui.end_row();
        egui::Grid::new("Last price:").striped(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                man_orders.last_price_s = *last_price;
                man_orders.last_price_buffer.push(last_price.clone());
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
                ui.end_row();
            });
        });
        ui.ctx().request_repaint();

        ui.end_row();
        let pp: f64 = man_orders.price_string.parse()?;
        let sp: f64 = man_orders.price_string.parse()?;

        egui::Grid::new("parent grid").striped(true).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.end_row();

                ui.end_row();
                let mut dummy: Option<f64> = None;
                ui.style_mut().visuals.selection.bg_fill = Color32::from_rgb(40, 40, 40);

                ui.horizontal(|ui| {
                    ui.selectable_value(&mut man_orders.quant, Quant::Q25, "25%");
                    ui.selectable_value(&mut man_orders.quant, Quant::Q50, "50%");
                    ui.selectable_value(&mut man_orders.quant, Quant::Q75, "75%");
                    ui.selectable_value(&mut man_orders.quant, Quant::Q100, "100%");
                    //ui.selectable_value(&mut dummy, None, "");
                });
                ui.end_row();

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
                /*
                let bb = man_orders.buy.clone();
                let qq = man_orders.quant.clone();
                */
                let bb = man_orders.buy;
                let qq = man_orders.quant;

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
                                stop_price: sp.clone() as f32,
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
                            stop_price: sp.clone() as f32,
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
                    //for hist tade this if fine but if live have this on the outside XXX
                    let o = man_orders.order.clone();
                    let res: Result<()> = match hist_trade {
                        Some(mut hist_trade) => {
                            let res1 = man_orders.check_order_price(&o, &last_price);
                            match res1 {
                                Ok(_) => {
                                    let res = man_orders.check_order_hist(&o, &mut hist_trade);
                                    match res {
                                        Ok(_) => {
                                            hist_trade.place_order(&o);
                                            Ok(())
                                        }
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(e) => Err(e),
                            }
                        }
                        None => {
                            todo!();
                        }
                    };
                    match res {
                        Ok(_) => {
                            man_orders.last_id += 1;
                            let oid = man_orders.last_id.clone();
                            man_orders.orders.insert(oid, (o, false));
                            //NOTE allow only one order for now...
                        }
                        Err(e) => {
                            tracing::error!["Invalid order:{:?}, error:{}", &o, e];
                        }
                    };
                }
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
        Ok(())
    }
}
use egui_extras::{Column, TableBuilder};
fn test_chart2(ui: &mut egui::Ui) {
    let red = Color32::from_rgb(255, 0, 0);
    let green = Color32::from_rgb(0, 255, 0);
    let name: String = "Candle".to_string();
    let width = 0.8 * 4.0;
    let divider = 60.0 * 4.0 * 4.0 * 6.0 * 4.0;
    let mut data_vec: Vec<f64> = vec![
        1372636800.0,
        1372723200.0,
        1372809600.0,
        1372896000.0,
        1372982400.0,
        1373068800.0,
        1373155200.0,
        1373241600.0,
        1373328000.0,
        1373414400.0,
        1373500800.0,
        1373587200.0,
    ];
    let mut data_vec2 = data_vec.clone();
    let test_datetime = chrono::NaiveDateTime::from_timestamp(1373587200, 0);
    let mut data = BoxPlot::new(
        &name,
        vec![
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(1.5, 2.2, 2.2, 2.6, 3.1),
            )
            .whisker_width(0.0)
            .fill(green)
            .stroke(Stroke::new(2.0, green))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(1.5, 2.4, 2.4, 2.8, 3.5),
            )
            .whisker_width(0.0)
            .fill(green)
            .stroke(Stroke::new(2.0, green))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(1.8, 2.0, 2.4, 2.5, 2.7),
            )
            .whisker_width(0.0)
            .fill(red)
            .stroke(Stroke::new(2.0, red))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(1.5, 1.8, 1.8, 2.1, 2.2),
            )
            .whisker_width(0.0)
            .fill(red)
            .stroke(Stroke::new(2.0, red))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(1.4, 1.6, 1.6, 1.8, 2.1),
            )
            .whisker_width(0.0)
            .fill(red)
            .stroke(Stroke::new(2.0, red))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(0.5, 1.5, 1.5, 1.6, 1.7),
            )
            .whisker_width(0.0)
            .fill(red)
            .stroke(Stroke::new(2.0, red))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(1.2, 1.4, 1.4, 2.9, 3.2),
            )
            .whisker_width(0.0)
            .fill(green)
            .stroke(Stroke::new(2.0, green))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(2.1, 2.3, 2.3, 2.6, 2.7),
            )
            .whisker_width(0.0)
            .fill(green)
            .stroke(Stroke::new(2.0, green))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(1.9, 2.1, 2.1, 2.7, 3.5),
            )
            .whisker_width(0.0)
            .fill(red)
            .stroke(Stroke::new(2.0, red))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(2.0, 2.1, 2.1, 2.9, 3.3),
            )
            .whisker_width(0.0)
            .fill(green)
            .stroke(Stroke::new(2.0, green))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(2.3, 2.9, 2.9, 3.7, 4.1),
            )
            .whisker_width(0.0)
            .fill(green)
            .stroke(Stroke::new(2.0, green))
            .name(format!["{}", test_datetime])
            .box_width(width),
            BoxElem::new(
                data_vec.pop().unwrap() / divider,
                BoxSpread::new(3.1, 3.4, 3.4, 4.0, 4.2),
            )
            .whisker_width(0.0)
            .fill(red)
            .stroke(Stroke::new(2.0, red))
            .name(format!["{}", test_datetime])
            .box_width(width),
        ],
    );
    let mut data2 = data.element_formatter(Box::new(time_format));
    let mut bar_data: Vec<Bar> = vec![
        Bar::new(data_vec2.pop().unwrap() / divider, 0.6)
            .fill(green)
            .stroke(Stroke::new(1.0, green))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.5)
            .fill(green)
            .stroke(Stroke::new(1.0, green))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.3)
            .fill(red)
            .stroke(Stroke::new(1.0, red))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.3)
            .fill(red)
            .stroke(Stroke::new(1.0, red))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.2)
            .fill(red)
            .stroke(Stroke::new(1.0, red))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.2)
            .fill(red)
            .stroke(Stroke::new(1.0, red))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.7)
            .fill(green)
            .stroke(Stroke::new(1.0, green))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.7)
            .fill(green)
            .stroke(Stroke::new(1.0, green))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.1)
            .fill(red)
            .stroke(Stroke::new(1.0, red))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.1)
            .fill(green)
            .stroke(Stroke::new(1.0, green))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 1.1)
            .fill(green)
            .stroke(Stroke::new(1.0, green))
            .width(width),
        Bar::new(data_vec2.pop().unwrap() / divider, 0.8)
            .fill(red)
            .stroke(Stroke::new(1.0, red))
            .width(width),
    ];
    let barchart = BarChart::new("bar volume chart", bar_data);

    let id = "test1";
    let plot = Plot::new("candlestick chart")
        .legend(Legend::default())
        .x_axis_formatter(x_format_1d)
        .link_cursor(id, [true, false])
        .link_axis(id, [true, false])
        .x_grid_spacer(uniform_grid_spacer(grid_spacer_1d))
        .width(360.0)
        .height(150.0)
        .default_x_bounds(59576.0, 59618.0)
        .default_y_bounds(0.0, 5.0);

    let plot2 = Plot::new("candlestick chart- boxchart")
        .legend(Legend::default())
        .x_axis_formatter(x_format_1d)
        .link_axis(id, [true, false])
        .link_cursor(id, [true, false])
        .x_grid_spacer(uniform_grid_spacer(grid_spacer_1d))
        .width(360.0)
        .height(50.0)
        .default_x_bounds(59576.0, 59618.0)
        .default_y_bounds(0.0, 5.0);

    //Check if update is true and if it is update the chart
    plot.show(ui, |plot_ui| {
        let red = Color32::from_rgb(255, 0, 0);
        let green = Color32::from_rgb(0, 255, 0);

        plot_ui.box_plot(data2);
        //ADD Hlines like dis
        let s = Stroke::new(0.5, green);
        let hline = HLine::new("test", 4);
        let hline2 = hline.stroke(s);
        plot_ui.add(hline2);

        //ADD Bar elements
        //plot_ui.bar_chart(barchart);
    });
    plot2.show(ui, |plot_ui| {
        plot_ui.bar_chart(barchart);
    });
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
struct LivePlot {
    live_asset_data: Arc<Mutex<AssetData>>,
    kline_plot: KlinePlot,
    search_string: String,
    symbol: String,
    intv: Intv,
    last_intv: Intv,
    lines: Vec<HLine>,
    reload: bool,
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
        }
    }
}

impl LivePlot {
    fn new(live_asset_data: Arc<Mutex<AssetData>>) -> Self {
        Self {
            live_asset_data,
            ..Default::default()
        }
    }
    fn show(
        live_plot: &mut LivePlot,
        plot_extras: &PlotExtras,
        cli_chan: watch::Sender<ClientInstruct>,
        live_price: &f64,
        collect_data: &HashMap<String, SymbolOutput>,
        ui: &mut egui::Ui,
    ) {
        //TODO clear this after debug
        //
        /*
        let cl_unlocked=live_plot.live_asset_data.clone();
        let unlocked=cl_unlocked.lock().expect("FFUCK");
        let id2=unlocked.id.clone();
        tracing::debug!["\x1b[36m Live asset data ID\x1b[0m: {:?}", &id2];
         */

        live_plot.kline_plot.show_live(
            ui,
            plot_extras,
            live_plot.live_asset_data.clone(),
            Some(collect_data),
        );

        tracing::trace!["\x1b[36m Collected Data\x1b[0m: {:?}", &collect_data];

        ui.end_row();
        if live_plot.intv != live_plot.last_intv {
            live_plot.reload = true;
            tracing::debug!["\x1b[36m live chart reloaded\x1b[0m: "];

            live_plot.last_intv = live_plot.intv;
            live_plot.kline_plot.intv = live_plot.intv;
            live_plot.reload = false;
        };
        egui::Grid::new("Hplot order assets:")
            .striped(true)
            .show(ui, |ui| {
                egui::ComboBox::from_label("Interval")
                    .selected_text(format!("{}", live_plot.intv.to_str()))
                    .show_ui(ui, |ui| {
                        for i in Intv::iter() {
                            ui.selectable_value(&mut live_plot.intv, i, i.to_str());
                        }
                    });
                let search = ui.add(
                    egui::TextEdit::singleline(&mut live_plot.search_string).hint_text("Search"),
                );
                ui.label("Search for asset symbol");
                let s = &live_plot.search_string.clone();
                let i = live_plot.intv;

                if ui.button("Search").clicked() {};

                if live_plot.reload == false {
                    //let ad = live_plot
                    //    .live_asset_data
                    //    .lock()
                    //    .expect("Live AD mutex poisoned! - LivePlot::show()");
                    //tracing::debug!["\x1b[36m Live chart reloaded intv: {}\x1b[0m: ", &i.to_str()];
                    //tracing::debug!["\x1b[36m Live chart reloaded intv: {}\x1b[0m: ", &live_plot.intv.to_str()];
                    //let res = live_plot.kline_plot.live_from_ad(&ad, s, i, 1_000, None);
                };
            });
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Copy, Clone, Dbg)]
struct HistExtras {
    last_price: f64,
}
impl Default for HistExtras {
    fn default() -> Self {
        Self { last_price: 0.0 }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
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
    hist_extras: Option<HistExtras>,
    hist_trade: HistTrade,

    navi_wicks: u16,
    trade_wicks: u16,

    navi_wicks_s: String,
    trade_wicks_s: String,

    pub trade_time: i64,
    pub att_klines: Option<Klines>,
    pub part_loaded: bool,
}

use crate::data::Klines;

use chrono::Datelike;
use std::time::{SystemTime, UNIX_EPOCH};

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
            trade_wicks: 20,

            navi_wicks_s: "200".to_string(),
            trade_wicks_s: "20".to_string(),

            trade_time: 0,
            att_klines: None,
            part_loaded: false,
        }
    }
}

use crate::data::DLAsset;

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg, Default)]
struct Account {
    pub enc_pub_key: Vec<u8>,
    pub enc_priv_key: Vec<u8>,

    api_key_enter_string: String,
    priv_api_key_enter_string: String,

    key_status: KeysStatus,

    balances: Vec<(String, f64)>,
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg, Default)]
pub enum KeysStatus {
    #[default]
    NotAdded,
    Invalid,
    Valid,
}
impl KeysStatus {
    fn to_str(&self) -> String {
        match self {
            KeysStatus::NotAdded => "Keys not added".to_string(),
            KeysStatus::Invalid => "Keys invalid".to_string(),
            KeysStatus::Valid => "Keys valid".to_string(),
        }
    }
}

impl Account {
    fn new() -> Self {
        Account {
            ..Default::default()
        }
    }
    fn show(account: &mut Account, cli_chan: watch::Sender<ClientInstruct>, ui: &mut egui::Ui) {
        egui::Grid::new("Account")
            .striped(true)
            .min_col_width(30.0)
            .show(ui, |ui| {
                ui.label(RichText::new(format!["BINANCE KEYS",]).color(Color32::YELLOW));
                ui.end_row();
                ui.add_sized(
                    egui::vec2(400.0, 20.0),
                    egui::TextEdit::singleline(&mut account.api_key_enter_string)
                        .hint_text("pub_key"),
                );
                ui.end_row();
                ui.add_sized(
                    egui::vec2(400.0, 20.0),
                    egui::TextEdit::singleline(&mut account.priv_api_key_enter_string)
                        .hint_text("priv_key"),
                );
                ui.end_row();
                if ui.button("Add keys").clicked() {
                    //TODO - add a function that stores keys securely here
                    account.api_key_enter_string = "".to_string();
                    account.priv_api_key_enter_string = "".to_string();
                };
                ui.end_row();
                if ui.button("Remove keys").clicked() {
                    //TODO - add a function that stores keys securely here
                    account.api_key_enter_string = "".to_string();
                    account.priv_api_key_enter_string = "".to_string();
                };
                //TODO get return status from validate_keys functon here and display it as such.
                //Save encrypted keys and settings in a savefile, that can be opened with password
                ui.end_row();
                match account.key_status {
                    KeysStatus::NotAdded => {
                        ui.label(
                            RichText::new(format!["Add binance API keys",]).color(Color32::ORANGE),
                        );
                    }
                    KeysStatus::Invalid => {
                        ui.label(RichText::new(format!["Keys Invalid",]).color(Color32::RED));
                    }
                    KeysStatus::Valid => {
                        ui.label(RichText::new(format!["Keys Valid",]).color(Color32::RED));
                    }
                };
            });
        ui.vertical(|ui| {
            let available_height = ui.available_height();
            let mut table = TableBuilder::new(ui)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
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
                        ui.strong("BalanceExchange");
                    });
                })
                .body(|mut body| {
                    for (asset, balance) in account.balances.iter() {
                        let row_height = 18.0;
                        body.row(row_height, |mut row| {
                            row.col(|ui| {
                                ui.label(format!["{}", asset]);
                            });
                            row.col(|ui| {
                                ui.label(format!["{}", balance]);
                            });
                        });
                    }
                });
        });
        egui::Grid::new("Account_balances")
            .striped(true)
            .min_col_width(30.0)
            .show(ui, |ui| {
                if ui.button("Refresh balances").clicked() {
                    //TODO if api keys installed call get_assets kind of function
                };
            });
    }
}
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Dbg)]
struct Settings {
    default_asset: String,
    default_intv: Intv,
}
impl Settings {
    fn new() -> Self {
        Settings {
            default_asset: "BTCUSDT".to_string(),
            default_intv: Intv::Min1,
        }
    }
    fn show(settings: &mut Settings, cli_chan: watch::Sender<ClientInstruct>, ui: &mut egui::Ui) {
        egui::Grid::new("Account")
            .striped(true)
            .min_col_width(30.0)
            .show(ui, |ui| {
                if ui.button("Settings").clicked() {};
            });
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
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
                /*
                egui::ComboBox::from_label("Download historical data for an asset:")
                    .selected_text(format!("Search result {}", data_manager.selected_coin))
                    .show_ui(ui, |ui| {
                        for i in data_manager.search_coin_shortlist.clone() {
                            ui.selectable_value(&mut data_manager.selected_coin, i.clone() , i.clone());
                        }
                    });
                */
                ui.add_sized(
                    egui::vec2(250.0, 20.0),
                    egui::TextEdit::singleline(&mut data_manager.coin_search_string)
                        .hint_text("Add asset to download list for binance"),
                );
                /*
                ui.add(
                    egui::TextEdit::singleline(&mut data_manager.coin_search_string)
                        .hint_text("Add asset to download list for binance"),
                );
                 * */
                if ui.button("Add").clicked() {
                    let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::InsertDLAsset {
                        symbol: data_manager.coin_search_string.clone(),
                        exchange: "Binance".to_string(),
                    });
                    cli_chan.send(msg);
                    data_manager.asset_list_loaded = false;
                };
                ui.end_row();
                if ui.button("Update all data").clicked() {
                    let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::UpdateDataAll);
                    cli_chan.send(msg);
                    data_manager.update_ran = true;
                    data_manager.update_success = true;
                    //TODO connect proper error handling...
                    data_manager.update_status = "Ran".to_string();
                };
            });
        /*
        egui::Grid::new("Data Manager Deletet").striped(true).show(ui, |ui| {
            egui::ComboBox::from_label("Delete historical data for an asset:")
                .selected_text(format!("Search downloaded result {}", data_manager.delete_selected_coin))
                .show_ui(ui, |ui| {
                    for i in data_manager.search_coin_shortlist.clone() {
                        ui.selectable_value(&mut data_manager.selected_coin, i.clone() , i.clone());
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut data_manager.coin_search_string)
                    .hint_text("Search for asset on Binance"),
            );
            if ui.button("Delete").clicked() {
                //NOTE Send client instruction
                let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::DelAsset{symbol:data_manager.delete_selected_coin.clone()});
                cli_chan.send(msg);
            }
        });
        */

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
            cli_chan.send(msg);
            //problem here is timing... this repeats the signal several times... but it works so
            //wtf...
            let ad = data_manager
                .hist_asset_data
                .lock()
                .expect("Unable to unlock mutex: DATA MANAGER");
            if ad.download_assets.is_empty==true{
            }else{
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
            let mut table = TableBuilder::new(ui)
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
                    for (asset) in data_manager.asset_list.iter() {
                        if asset.asset != "TEST"{
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
                                        chrono::NaiveDateTime::from_timestamp_millis(asset.dat_start_t)
                                    {
                                        ui.label(format!["{}", start_time]);
                                    } else {
                                        ui.label(format!["NaN"]);
                                    };
                                });
                                row.col(|ui| {
                                    if let Some(end_time) =
                                        chrono::NaiveDateTime::from_timestamp_millis(asset.dat_end_t)
                                    {
                                        ui.label(format!["{}", end_time]);
                                    } else {
                                        ui.label(format!["NaN"]);
                                    };
                                });
                                row.col(|ui| if ui.button("Delete").clicked() {
                                    let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::DelAsset{symbol:asset.asset.clone()});
                                    cli_chan.send(msg);
                                    data_manager.asset_list_loaded = false;
                                });
                            });
                        };
                    }
                });
        });
    }
}

fn toggle_ui_compact(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired_size = ui.spacing().interact_size.y * egui::vec2(2.0, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();
        ui.painter().rect(
            rect,
            radius,
            visuals.bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let center = egui::pos2(circle_x, rect.center().y);
        ui.painter()
            .circle(center, 0.75 * radius, visuals.bg_fill, visuals.bg_stroke);
    }

    response
}

enum LineStyle {
    Solid(f32),
    Dotted(f32),
}

enum LineState {
    ActiveColor(Color32),
    InactiveColor(Color32),
}

enum HlineType {
    BuyOrder((LineState, LineStyle)),
    SellOrder((LineState, LineStyle)),
    LastPrice((LineStyle, Color32)),
}

const buy_active: LineState = LineState::ActiveColor(Color32::GREEN);
const buy_inactive: LineState = LineState::InactiveColor(Color32::GREEN);
const buy_style: LineStyle = LineStyle::Solid(0.5);

const sell_active: LineState = LineState::ActiveColor(Color32::RED);
const sell_inactive: LineState = LineState::InactiveColor(Color32::RED);
const sell_style: LineStyle = LineStyle::Solid(0.5);

const last_price_color: Color32 = Color32::YELLOW;
const last_price_line: LineStyle = LineStyle::Dotted(0.5);

impl HlineType {
    fn hline_order(o: &Order, active: bool) -> HLine {
        let side = o.get_side();
        let price = o.get_price();
        if side == true {
            if active == true {
                return HlineType::BuyOrder((buy_active, buy_style)).to_hline(price);
            } else {
                return HlineType::BuyOrder((buy_inactive, buy_style)).to_hline(price);
            }
        } else {
            if active == true {
                return HlineType::SellOrder((sell_active, sell_style)).to_hline(price);
            } else {
                return HlineType::SellOrder((sell_inactive, sell_style)).to_hline(price);
            }
        }
    }
    fn to_hline(&self, value: &f64) -> HLine {
        match &self {
            HlineType::BuyOrder((ba, bs)) => {
                let color = match ba {
                    LineState::ActiveColor(color) => color,
                    LineState::InactiveColor(color) => color,
                };
                let width = match bs {
                    LineStyle::Solid(width) => width,
                    LineStyle::Dotted(width) => width,
                };
                let s = Stroke::new(width.clone(), color.clone());
                HLine::new("Buy order", value.clone()).stroke(s)
            }
            HlineType::SellOrder((si, ss)) => {
                let color = match si {
                    LineState::ActiveColor(color) => color,
                    LineState::InactiveColor(color) => color,
                };
                let width = match ss {
                    LineStyle::Solid(width) => width,
                    LineStyle::Dotted(width) => width,
                };
                let s = Stroke::new(width.clone(), color.clone());
                HLine::new("Sell order", value.clone()).stroke(s)
            }
            HlineType::LastPrice((l, color)) => {
                let width = match l {
                    LineStyle::Solid(width) => width,
                    LineStyle::Dotted(width) => width,
                };
                let s = Stroke::new(width.clone(), color.clone());
                HLine::new("Last price", value.clone()).stroke(s)
            }
        }
    }
}

impl HistPlot {
    fn create_hlines(&self, order_price: &[(Order, bool)]) -> Vec<HLine> {
        order_price
            .iter()
            .map(|(order, active)| HlineType::hline_order(order, *active))
            .collect()
    }
    fn new(hist_asset_data: Arc<Mutex<AssetData>>) -> Self {
        Self {
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
    ) {
        if hist_plot.part_loaded == true {
            if let Some(ref klines) = hist_plot.att_klines {
                hist_plot.kline_plot.add_hist(&klines);
            };
        };
        hist_plot
            .kline_plot
            .show_live(ui, plot_extras, hist_ad, None);

        if hist_plot.intv != hist_plot.last_intv {
            hist_plot.last_intv = hist_plot.intv;
            hist_plot.kline_plot.intv = hist_plot.intv;
        };

        ui.end_row();
        egui::Grid::new("Hplot order assets:")
            .striped(true)
            .show(ui, |ui| {
                egui::ComboBox::from_label("Interval")
                    .selected_text(format!("{}", hist_plot.intv.to_str()))
                    .show_ui(ui, |ui| {
                        for i in Intv::iter() {
                            ui.selectable_value(&mut hist_plot.intv, i, i.to_str());
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut hist_plot.search_load_string)
                        .hint_text("Search for asset"),
                );
                /*
                if ui.button("Search").clicked() {
                    let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::LoadHistFull{
                        symbol: hist_plot.search_load_string.clone(), start:st, end:et
                    });
                }
                 * */
                ui.label(RichText::new(format!["Jump to:"]).color(Color32::WHITE));
                ui.add(
                    egui_extras::DatePickerButton::new(&mut hist_plot.picked_date)
                        .id_salt("trade_time"),
                );
                ui.add(
                    egui_extras::DatePickerButton::new(&mut hist_plot.picked_date)
                        .id_salt("hist_start"),
                );
                ui.add(
                    egui_extras::DatePickerButton::new(&mut hist_plot.picked_date_end)
                        .id_salt("hist_end"),
                );
                ui.end_row();
                if ui.button("Trade time").clicked() {}
                if ui.button("Load Asset - part data").clicked() {
                    let st = match hist_plot.picked_date.and_hms_opt(0, 0, 0) {
                        Some(st) => st,
                        None => {
                            tracing::error!["GUI: could not parse picked date!"];
                            chrono::NaiveDateTime::default()
                        }
                    }
                    .timestamp_millis();
                    let et = match hist_plot.picked_date_end.and_hms_opt(0, 0, 0) {
                        Some(st) => st,
                        None => {
                            tracing::error!["GUI: could not parse picked date!"];
                            chrono::NaiveDateTime::default()
                        }
                    }
                    .timestamp_millis();
                    let msg = ClientInstruct::SendSQLInstructs(SQLInstructs::LoadHistDataPart {
                        symbol: hist_plot.search_load_string.clone(),
                        start: st,
                        end: et,
                    });
                    hist_plot.part_loaded = true;
                    cli_chan.send(msg);
                }
                if ui.button("Load Asset - all data").clicked() {
                    let s = hist_plot.search_load_string.clone();
                    let ad = hist_plot
                        .hist_asset_data
                        .lock()
                        .expect("Posoned mutex! - Hist asset data");

                    //NOTE validate assed DL before checking
                    hist_plot.kline_plot.symbol = s.clone();

                    let res = ad.kline_data.get(&s.clone());
                    match res {
                        Some(_) => {
                            tracing::error!["Data for asset {} already loaded!", &s];
                        }
                        None => {
                            let msg =
                                ClientInstruct::SendSQLInstructs(SQLInstructs::LoadHistData {
                                    symbol: hist_plot.search_load_string.clone(),
                                });
                            cli_chan.send(msg);
                        }
                    };
                }
                ui.end_row();
                /*
                let search = ui.add(
                    egui::TextEdit::singleline(&mut hist_plot.loaded_search_string)
                        .hint_text("Search for loaded asset"),
                );
                if ui.button("Show").clicked() {
                    if hist_plot.part_loaded==false{
                        tracing::debug!["Part loaded: false show ran!"];
                        let s = &hist_plot.loaded_search_string.clone();
                        let ad = hist_plot
                            .hist_asset_data
                            .lock()
                            .expect("Posoned mutex! - Hist asset data");
                        let i = hist_plot.kline_plot.intv;
                        let res = hist_plot.kline_plot.live_from_ad(
                            &ad, s, i, 1_000,
                            None, //hist_plot.picked_date.and_time(time), hist_plot.picked_date_end.and_time(time))
                        );
                        match res{
                            Ok(_)=>{},
                            Err(e)=>tracing::error!["Load all data error: {}",e],
                        }
                    };
                }
                 * */
                let search = ui.add(
                    egui::TextEdit::singleline(&mut hist_plot.unload_search_string)
                        .hint_text("Unload asset"),
                );
                if ui.button("Unload").clicked() {
                    let s = &hist_plot.unload_search_string.clone();
                    let mut ad = hist_plot
                        .hist_asset_data
                        .lock()
                        .expect("Posoned mutex! - Hist asset data");
                    let res = ad.kline_data.get(&s.clone());
                    match res {
                        Some(_) => {
                            tracing::info!["Remove data for asset {}", &s];
                            ad.kline_data.remove(s);
                        }
                        None => tracing::error!["Data for asset {} not loaded!", &s],
                    };
                };
            });
        egui::Grid::new("Hplot forwards:").show(ui, |ui| {
            if ui.button("Trade >>").clicked() {
                let res: Result<u16, ParseIntError> = hist_plot.trade_wicks_s.parse();
                let n_wicks = match res {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!["Parsing error for trade wicks: {}", e];
                        0
                    }
                };
                hist_plot.navi_wicks = n_wicks;
                //TODO change trade time whenever symbol is changed to latest data point
                //
                hist_plot.hist_trade.trade_time = hist_plot.trade_time;
                let res = hist_plot.hist_trade.trade_forward(n_wicks);
                hist_plot.hist_trade.trade_time =
                    hist_plot.trade_time + hist_plot.intv.to_ms() * (n_wicks as i64);
            }
            let search = ui.add(
                egui::TextEdit::singleline(&mut hist_plot.trade_wicks_s).hint_text("Trade N wicks"),
            );
        });
    }
}
use std::num::ParseIntError;

use tracing::instrument;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample() {
        assert!(true);
    }
}
