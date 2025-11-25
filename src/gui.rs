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

use tokio::sync::watch;

use eframe::egui::{self, DragValue, Event, Vec2};

use crate::{ClientInstruct,ClientResponse, SQLResponse,ProcResp, SQLInstructs};

use egui::{Color32, ComboBox, epaint};
use egui_plot_bintrade::{
    Bar, BarChart, BoxElem, BoxPlot, BoxSpread, GridInput, GridMark, HLine,
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
    tick_kline: Option<(chrono::NaiveDateTime, f64, f64, f64, f64, f64)>,

    intv: Intv,
    name: String,
    loading: bool,
    static_loaded: bool,
    chart_params: (f64, f64),
    x_bounds: (f64, f64),
    y_bounds: (f64, f64),

    hlines:Vec<HLine>
}
impl Default for KlinePlot {
    fn default() -> Self {
        Self {
            l_boxplot: vec![],
            l_barchart: vec![],
            tick_kline: None,
            intv: Intv::Min15,
            name: "".to_string(),
            loading: false,
            static_loaded: false,
            chart_params: (1.0, 1.0),
            x_bounds: (0.0, 100.0),
            y_bounds: (0.0, 100.0),

            hlines:vec![],

        }
    }
}
impl KlinePlot {
    fn show_empty(&self, ui: &mut egui::Ui) {
        let plot = self.mk_plt();
        let bp = BoxPlot::new(&self.name, vec![]);
        plot.show(ui, |plot_ui| {
            plot_ui.box_plot(bp);
        });
    }
    fn mk_plt(&self) -> Plot {
        let (x_lower, x_higher) = self.x_bounds;
        let (y_lower, y_higher) = self.y_bounds;
        make_plot(&self.name, self.intv, y_higher * 1.25, x_lower, x_higher)
    }
    fn add_live(
        &mut self,
        kline_input: &[(chrono::NaiveDateTime, f64, f64, f64, f64, f64)],
        divider: &f64,
        width: &f64,
    ) {
        self.l_boxplot = vec![];
        self.l_barchart = vec![];
        let mut highest: f64 = 0.0;
        let t0 = kline_input[0].0;
        let t1 = kline_input[kline_input.len() - 1].0;
        self.x_bounds = ((t0.timestamp() as f64)*0.99993/self.chart_params.0, (t1.timestamp() as f64)*1.00007/self.chart_params.0);
        for kline in kline_input.iter() {
            let (_, _, h, _, _, _) = kline;
            if h > &highest {
                highest = h.clone();
            };
            let (boxe, bar) = box_element(kline, divider, width);
            self.l_boxplot.push(boxe);
            self.l_barchart.push(bar);
        }
        self.y_bounds = (0.0, highest);
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
    fn show(&mut self, ui: &mut egui::Ui, plot_extras:&PlotExtras) -> Result<()> {
        let plot = self.mk_plt();
        //Handle live tick as separate box...
        let bp = BoxPlot::new(&self.name, self.l_boxplot.clone())
            .element_formatter(Box::new(time_format));
        let bc = BarChart::new(&self.name, self.l_barchart.clone());
        let hlines=self.hlines.clone();
        if let Some(tick_kline) = self.tick_kline {
            let (tick_box_e, tick_vol) = box_element(
                &tick_kline,
                &self.chart_params.0,
                &self.chart_params.1,
            );
            let tick_box_plot = vec![tick_box_e];
            let tick_bar_vol = vec![tick_vol];
            let bp_tick = BoxPlot::new("Tick", tick_box_plot);
            let bc_tick = BarChart::new("Tick Vol", tick_bar_vol);
            if self.loading == false && self.static_loaded == true {
                plot.show(ui, |plot_ui| {
                    plot_ui.box_plot(bp);
                    plot_ui.bar_chart(bc);
                    plot_ui.box_plot(bp_tick);
                    plot_ui.bar_chart(bc_tick);
                });
            } else {
                self.show_empty(ui);
            }
        } else {
            if self.loading == false && self.static_loaded == true {
                plot.show(ui, |plot_ui| {
                    log::debug!["Hlines in plot: {:?}",&hlines];
                    if hlines!= []{
                        for h in hlines{
                            log::debug!["Hlines (for h in ){:?}",&h];
                            plot_ui.add(h);
                        };
                    };
                    plot_ui.box_plot(bp);
                    plot_ui.bar_chart(bc);
                });
            } else {
                self.show_empty(ui);
            }
        }
        Ok(())
    }
    fn live_from_sql() { //TODO load a designated period directly from sql... maybe better...
    }
    //#[instrument(level="debug")]
    fn live_from_ad(
        &mut self,
        ad: &AssetData,
        symbol: &str,
        intv: Intv,
        max_load_points: usize,
        timestamps:Option<(chrono::NaiveDateTime,chrono::NaiveDateTime)>
    ) -> Result<()> {
        //tracing::debug!["Show live data from AD asset data={:?}",ad.debug()];
        let k = match timestamps{
            Some((start,end))=>{
                ad.find_slice(symbol, &intv, &start, &end)
                    .ok_or(anyhow!["Unable to find slice in the period:{} to {}", &start,&end])?
            }
            None=>ad.load_full_intv(symbol, &intv)?

        };
        let (div, width) = get_chart_params(&intv);
        self.chart_params = (div, width);
        if k.len() <= max_load_points {
            self.loading = true;
            self.add_live(k, &div, &width);
            self.loading = false;
        } else {
            //tracing::debug!["max load points separation{:?}",k];
            let kl = &k[(k.len() - max_load_points)..];
            //tracing::debug!["max load points separation{:?}",kl];
            self.loading = true;
            self.add_live(kl, &div, &width);
            self.loading = false;
        }
        self.static_loaded = true;
        return Ok(());
    }
    fn from_watch_channel(
        &mut self,
        mut kline_chan: watch::Receiver<(
            chrono::NaiveDateTime,
            f64,
            f64,
            f64,
            f64,
            f64,
        )>,
    ) {
        let kl = kline_chan.borrow_and_update().clone();
        let (div, width) = self.chart_params;
        self.add_live_single(&kl, &div, &width);
    }
    fn tick_from_watch_channel(
        &mut self,
        mut kline_chan: watch::Receiver<(
            chrono::NaiveDateTime,
            f64,
            f64,
            f64,
            f64,
            f64,
        )>,
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
    let a1 = (high - low) / 2.0;
    let red = Color32::from_rgb(255, 0, 0);
    let green = Color32::from_rgb(0, 255, 0);
    let width = width.clone();
    let volume_mod = 2.5;
    if open >= close {
        let bb: BoxElem = BoxElem::new(
            (time.timestamp() as f64) / divider,
            BoxSpread::new(low, close, a1, open, high),
        )
        .whisker_width(0.0)
        .fill(green)
        .stroke(Stroke::new(2.0, red))
        .name(format!["{}", time])
        .box_width(width);
        let b = Bar::new(time.timestamp() as f64, volume * volume_mod)
            .fill(green)
            .stroke(Stroke::new(1.0, green))
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
        let b = Bar::new(time.timestamp() as f64, volume * volume_mod)
            .fill(red)
            .stroke(Stroke::new(1.0, red))
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
    ( $($name:ident, $formatter:ident, $formatter2:ident, $y_upper:ident, $x_lower:ident, $x_higher:ident),* ) => {
        {
            let plot = Plot::new($($name.to_string())*)
                .legend(Legend::default())
                .x_axis_formatter($($formatter)*)
                //.x_grid_spacer(uniform_grid_spacer($($formatter2)*))
                //TODO BUG if this is not here... the gui doesn't crash...
                .default_y_bounds(0.0,$($y_upper)*)
                .default_x_bounds($($x_lower)*,$($x_higher)*)
                .height(320.0)
                .view_aspect(2.0);

            plot
        }
    };
}
fn make_plot(
    name: &str,
    intv: Intv,
    y_higher: f64,
    x_lower: f64,
    x_higher: f64,
) -> Plot {
    match intv {
        Intv::Min1 => make_p!(
            name,
            x_format_1min,
            grid_spacer_1min,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Min3 => make_p!(
            name,
            x_format_3min,
            grid_spacer_3min,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Min5 => make_p!(
            name,
            x_format_5min,
            grid_spacer_5min,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Min15 => make_p!(
            name,
            x_format_15min,
            grid_spacer_15min,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Min30 => make_p!(
            name,
            x_format_30min,
            grid_spacer_30min,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Hour1 => make_p!(
            name,
            x_format_1h,
            grid_spacer_1h,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Hour2 => make_p!(
            name,
            x_format_2h,
            grid_spacer_2h,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Hour4 => make_p!(
            name,
            x_format_4h,
            grid_spacer_4h,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Hour6 => make_p!(
            name,
            x_format_6h,
            grid_spacer_6h,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Hour8 => make_p!(
            name,
            x_format_8h,
            grid_spacer_8h,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Hour12 => make_p!(
            name,
            x_format_12h,
            grid_spacer_12h,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Day1 => make_p!(
            name,
            x_format_1d,
            grid_spacer_1d,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Day3 => make_p!(
            name,
            x_format_3d,
            grid_spacer_3d,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Week1 => make_p!(
            name,
            x_format_1w,
            grid_spacer_1w,
            y_higher,
            x_lower,
            x_higher
        ),
        Intv::Month1 => todo!(),
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

//TODO fuck with these to fix the graph or give up and fork
const m1_div: i64 = 60;
const m3_div: i64 = 60;
const m5_div: i64 = 60;
const m15_div: i64 = 60 * 4;
const m30_div: i64 = 60 * 4;
const h1_div: i64 = 60 * 4 * 4;
const h2_div: i64 = 60 * 4 * 4 * 2;
const h4_div: i64 = 60 * 4 * 4 * 4;
const h6_div: i64 = 60 * 4 * 4 * 6;
const h8_div: i64 = 60 * 4 * 4 * 8;
const h12_div: i64 = 60 * 4 * 4 * 12;
const d1_div: i64 = 60 * 4 * 4 * 6;
const d3_div: i64 = 60 * 4 * 4 * 6;
const w1_div: i64 = 60 * 4 * 4 * 6;

const gap1:f64=0.8;
const gap2:f64=0.8 * 4.0;
fn get_chart_params(intv: &Intv) -> (f64, f64) {
    match intv {
        Intv::Min1 => (m1_div as f64, gap1), //incorrect
        Intv::Min3 => (m3_div as f64, gap1),
        Intv::Min5 => (m5_div as f64, gap1),
        Intv::Min15 => (m15_div as f64, gap2),
        Intv::Min30 => (m30_div as f64, gap2),
        Intv::Hour1 => (h1_div as f64, gap2 ),
        Intv::Hour2 => (h2_div as f64, gap2 ),
        Intv::Hour4 => (h4_div as f64, gap2 ),
        Intv::Hour6 => (h6_div as f64, gap2 ),
        Intv::Hour8 => (h8_div as f64, gap2 ),
        Intv::Hour12 => (h12_div as f64, gap2),
        Intv::Day1 => (d1_div as f64, gap2),
        Intv::Day3 => (d3_div as f64, gap2),
        Intv::Week1 => (w1_div as f64, gap2),
        Intv::Month1 => todo!(), //with reference to 1970 1,1 00:00 perhaps?
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

fn time_format(input: &BoxElem, plot: &BoxPlot) -> String {
    format!(
        "Time: {} \n High: {} \n Low: {} \n Average: {}",
        input.name,
        input.spread.upper_whisker,
        input.spread.lower_whisker,
        input.spread.median
    )
}

//NOTE interval formatter 2 values = width (0.8 for 1min), divider (60 for 1 min), spacer(60 60 1
//for 1min)

#[derive(
    EnumIter,
    Debug,
    Clone,
    Copy,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]

enum PaneType {
    None,
    LiveTrade,
    HistTrade,
}
impl fmt::Display for PaneType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            PaneType::LiveTrade => write!(f, "Live Trade"),
            PaneType::HistTrade => write!(f, "Hist Trade"),
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
        format!("Pane {}", pane.ty).into()
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
                test_chart(ui);
            }
            PaneType::LiveTrade => {
                let chan =
                    self.send_to_cli.clone().expect("Cli comm channel none!");
                let live_price =
                    self.live_price.lock().expect("Live price mutex poisoned!");
                let live_plot_l = self.live_plot.clone();
                let c_data=self.collect_data.lock().expect("Live collect data mutex poisoned!");

                let p_extras:PlotExtras=PlotExtras::None;
                let mut live_plot =
                    live_plot_l.lock().expect("Live plot mutex posoned!");
                //NOTE... see if chan works too... should... 
                LivePlot::show(&mut live_plot, &p_extras, chan, &live_price, &c_data, ui);
                let mut man_orders = self
                    .man_orders
                    .get_mut(&pane.nr)
                    .expect("Manual order window not found!");
                if man_orders.plot_extras.is_some() != true{
                    man_orders.plot_extras=Some(p_extras);
                };
                let chan =
                    self.send_to_cli.clone().expect("Cli comm channel none!");
                ManualOrders::show(&mut man_orders, &live_price, None, chan, ui, None);
            }
            PaneType::HistTrade => {
                let sql_resps:Option<&Vec<ClientResponse>>=match self.resp_buff.as_ref(){
                    Some(a)=>{
                        a.get(&ProcResp::SQLResp(SQLResponse::None))
                    }
                    None => None 
                };
                let chan =
                    self.send_to_cli.clone().expect("Cli comm channel none!");
                let hist_extras=HistExtras::default();
                let p_extras:PlotExtras=PlotExtras::None;
                let mut h_plot = self
                    .hist_plot
                    .get_mut(&pane.nr)
                    .expect("Hist plot gui struct not found!");
                if h_plot.hist_extras.is_some() != true{
                    h_plot.hist_extras=Some(hist_extras);
                };
                HistPlot::show(&mut h_plot, &p_extras, chan, ui);
                let mut man_orders = self
                    .man_orders
                    .get_mut(&pane.nr)
                    .expect("Manual order window not found!");
                if man_orders.plot_extras.is_some() != true{
                    man_orders.plot_extras=Some(p_extras);
                };
                let chan =
                    self.send_to_cli.clone().expect("Cli comm channel none!");
                ManualOrders::show(&mut man_orders, &hist_extras.last_price, Some(&mut h_plot.hist_trade), chan, ui, Some(&mut h_plot.kline_plot.hlines));
            }
        };
        return response;
    }
    fn on_tab_close(
        //TODO - remove old structs form maps here...
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
                    }
                    let tab_title = self.tab_title_for_pane(pane);
                    log::debug!(
                        "Closing tab: {}, tile ID: {tile_id:?}",
                        tab_title.text()
                    );
                }
                Tile::Container(container) => {
                    log::debug!("Closing container: {:?}", container.kind());
                    let children_ids = container.children();
                    for child_id in children_ids {
                        if let Some(Tile::Pane(pane)) = tiles.get(*child_id) {
                            let tab_title = self.tab_title_for_pane(pane);
                            log::debug!(
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
    let mut next_view_nr = 0;
    let mut gen_pane = || {
        let pane = Pane {
            nr: next_view_nr,
            ..Default::default()
        };
        next_view_nr += 1;
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

//NOTE simply load the SQL asset table and see which studies are completed on which data...
//load asset list...
//search asset and get info...
//start asset download process
//load assset
//unload asset

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
    collect_data:Arc<Mutex<HashMap<String,SymbolOutput>>>,

    live_plot: Rc<Mutex<LivePlot>>,

    lp_chan_recv:watch::Receiver<f64>,

    //NOTE... when closing windows find a way to remove the entry from the map to stop hogging
    //memory
    send_to_cli: Option<watch::Sender<ClientInstruct>>,
    recv_from_cli: Option<watch::Receiver<ClientResponse>>,

    resp_buff:Option<HashMap<ProcResp, Vec<ClientResponse>>>,
    last_resp:Option<ClientResponse>,

    man_orders: BTreeMap<usize, ManualOrders>,
    hist_plot: BTreeMap<usize, HistPlot>,
}


impl DesktopApp{
    fn update_procresp(last_resp:&mut ClientResponse,recv:&mut watch::Receiver<ClientResponse>, buff:&mut HashMap<ProcResp,Vec<ClientResponse>>){
        let resp=recv.borrow_and_update().clone();
        if &resp != last_resp{
            match resp{
                ClientResponse::None=>{},
                ClientResponse::Success | ClientResponse::Failure(_)=>{
                    let res=buff.get(&ProcResp::Client);
                    match res{
                        Some(_)=>{
                            let resp_vec=buff.get_mut(&ProcResp::Client).expect("Unable to find cli_resp vector!");
                            resp_vec.push(resp.clone())
                        }
                        None=>{
                            buff.insert(ProcResp::Client,vec![]);
                        }
                    }
                },
                ClientResponse::ProcResp(ref proc_resp)=>{
                    let res=buff.get(&proc_resp);
                    match res{
                        Some(_)=>{
                            let resp_vec=buff.get_mut(&proc_resp).expect("Unable to find proc_resp vector!");
                            resp_vec.push(resp.clone());
                        }
                        None=>{
                            buff.insert(proc_resp.clone(),vec![]);
                        }
                    }
                },
            }
            *last_resp=resp;
        }
    }
}

#[derive(Dbg)]
struct TaskManager{
    instruct:ClientInstruct,
    responses:Vec<ClientResponse>,
}

impl Default for TaskManager{
    fn default()->Self{
        Self{
            instruct:ClientInstruct::None,
            responses:vec![],
        }
    }
}

impl TaskManager{
    fn new()->Self{
        TaskManager::default()
    }
    fn show(mut task_manager:&mut TaskManager, cli_chan:watch::Sender<ClientInstruct>, recver:watch::Receiver<ClientResponse>, ui: &mut egui::Ui ){
        egui::Grid::new("TaskManager")
            .striped(true)
            .show(ui, |ui| {
                egui::ComboBox::from_label("CliInstruct")
                    .selected_text(format!("{}", task_manager.instruct))
                    .show_ui(ui, |ui| {
                        for i in ClientInstruct::iter() {
                            ui.selectable_value(
                                &mut task_manager.instruct,
                                i.clone(),
                                i.clone().to_str(),
                            );
                        }
                    });
                if ui.button("Send").clicked() {
                    cli_chan.send(task_manager.instruct.clone());
                }
            });
    }
}
//NOTE all of persistent gui data has to be on Desktop App. TODO Get rid of Arc?
impl Default for DesktopApp {
    fn default() -> Self {
        // NOTE this struct should never be initiated this way, all refs need to be passed
        // using :
        let hat = Rc::new(Mutex::new(HistPlot::default()));
        let lp = Arc::new(Mutex::new(0.0));
        let asset_data = Arc::new(Mutex::new(AssetData::new(6661)));
        let hist_asset_data = Arc::new(Mutex::new(AssetData::new(6662)));
        let cd= Arc::new(Mutex::new(HashMap::new()));

        let (_,lp_chan_recv)=watch::channel(0.0);
        Self {
            last_resp:Some(ClientResponse::None),
            resp_buff:Some(HashMap::new()),
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
            zoom_speed: 1.0,//
            scroll_speed: 1.0,

            //Cli refs
            live_price: lp,
            asset_data,
            hist_asset_data,
            collect_data:cd,

            lp_chan_recv,

            live_plot: Rc::new(Mutex::new(LivePlot::default())),

            //Channels
            send_to_cli: None,
            recv_from_cli: None,

            man_orders: BTreeMap::new(),
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
        collect_data: Arc<Mutex<HashMap<String,SymbolOutput>>>,
        lp_chan_recv:watch::Receiver<f64>
    ) -> Self {
        cc.storage;
        let live_plot = Rc::new(Mutex::new(LivePlot::new(asset_data.clone())));
        Self {
            send_to_cli: Some(schan),
            recv_from_cli: Some(rchan),
            live_price,
            live_plot,
            collect_data,
            asset_data,
            hist_asset_data,
            lp_chan_recv,
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
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        //TODO cose the rest of the program from here by sending cancel token and
                        //saving everything
                    }
                });
                ui.add_space(16.0);
                let mut next_panel_type: PaneType = PaneType::None;
                ComboBox::from_label("")
                    .selected_text(format!("Tools"))
                    .show_ui(ui, |ui| {
                        for pty in PaneType::iter() {
                            ui.selectable_value(
                                &mut next_panel_type,
                                pty,
                                format!["{}", pty],
                            );
                        }
                    });
                if next_panel_type != PaneType::None {
                    if let Some(parent) = self.add_child_to.take() {
                        let mut tree = self
                            .tree
                            .lock()
                            .expect("Posoned mutex on pane tree!");
                        let mut new_child = tree.tiles.insert_pane(Pane::new(
                            self.pane_number,
                            PaneType::None,
                        ));
                        match next_panel_type {
                            PaneType::None=> {}
                            PaneType::LiveTrade => {
                                self.man_orders.insert(
                                    self.pane_number + 1,
                                    ManualOrders::default(),
                                );

                                new_child = tree.tiles.insert_pane(Pane::new(
                                    self.pane_number + 1,
                                    PaneType::LiveTrade,
                                ));
                                self.pane_number += 1;
                            }
                            PaneType::HistTrade => {
                                self.man_orders.insert(
                                    self.pane_number + 1,
                                    ManualOrders::default(),
                                );
                                let ad=self.hist_asset_data.lock().expect("Debug mutex");
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
                        }
                        if let Some(egui_tiles::Tile::Container(
                            egui_tiles::Container::Tabs(tabs),
                        )) = tree.tiles.get_mut(parent)
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
            let mut last_resp=self.last_resp.take().expect("Unable to get last_resp");;
            let mut recv=self.recv_from_cli.take().expect("Unable to get recv_from_client channel!");
            let mut buff=self.resp_buff.take().expect("Unable to get resp_buff");
            DesktopApp::update_procresp(&mut last_resp,&mut recv,&mut buff);
            self.recv_from_cli=Some(recv);
            self.resp_buff=Some(buff);
            self.last_resp=Some(last_resp);

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

/////////////////////////////////////NOTE RAW WIDGETS///////////////////////////////////XXX
use egui::Id;

#[derive(Clone, PartialEq, Eq)]
#[derive(Dbg)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct DragAndDropDemo {
    /// columns with items
    columns: Vec<Vec<String>>,
}

impl Default for DragAndDropDemo {
    fn default() -> Self {
        Self {
            columns: vec![
                vec!["Item A", "Item B", "Item C", "Item D"],
                vec!["Item E", "Item F", "Item G"],
                vec!["Item H", "Item I", "Item J", "Item K"],
            ]
            .into_iter()
            .map(|v| v.into_iter().map(ToString::to_string).collect())
            .collect(),
        }
    }
}

/// What is being dragged.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
struct Location {
    col: usize,
    row: usize,
}

impl DragAndDropDemo {
    fn name(&self) -> &'static str {
        "✋ Drag and Drop"
    }

    fn show(&mut self, ctx: &EguiContext, open: &mut bool) {
        Window::new(self.name())
            .open(open)
            .default_size(vec2(256.0, 256.0))
            .vscroll(false)
            .resizable(false)
            .show(ctx, |ui| self.ui(ui));
    }
    fn ui(&mut self, ui: &mut Ui) {
        ui.label("This is a simple example of drag-and-drop in egui.");
        ui.label("Drag items between columns.");

        // If there is a drop, store the location of the item being dragged, and the destination for the drop.
        let mut from = None;
        let mut to = None;

        ui.columns(self.columns.len(), |uis| {
            for (col_idx, column) in
                self.columns.clone().into_iter().enumerate()
            {
                let ui = &mut uis[col_idx];

                let frame = Frame::default().inner_margin(4.0);

                let (_, dropped_payload) =
                    ui.dnd_drop_zone::<Location, ()>(frame, |ui| {
                        ui.set_min_size(vec2(64.0, 100.0));
                        for (row_idx, item) in column.iter().enumerate() {
                            let item_id = Id::new((
                                "my_drag_and_drop_demo",
                                col_idx,
                                row_idx,
                            ));
                            let item_location = Location {
                                col: col_idx,
                                row: row_idx,
                            };
                            let response = ui
                                .dnd_drag_source(item_id, item_location, |ui| {
                                    ui.label(item);
                                })
                                .response;

                            // Detect drops onto this item:
                            if let (Some(pointer), Some(hovered_payload)) = (
                                ui.input(|i| i.pointer.interact_pos()),
                                response.dnd_hover_payload::<Location>(),
                            ) {
                                let rect = response.rect;

                                // Preview insertion:
                                let stroke =
                                    egui::Stroke::new(1.0, Color32::WHITE);
                                let insert_row_idx =
                                    if *hovered_payload == item_location {
                                        // We are dragged onto ourselves
                                        ui.painter().hline(
                                            rect.x_range(),
                                            rect.center().y,
                                            stroke,
                                        );
                                        row_idx
                                    } else if pointer.y < rect.center().y {
                                        // Above us
                                        ui.painter().hline(
                                            rect.x_range(),
                                            rect.top(),
                                            stroke,
                                        );
                                        row_idx
                                    } else {
                                        // Below us
                                        ui.painter().hline(
                                            rect.x_range(),
                                            rect.bottom(),
                                            stroke,
                                        );
                                        row_idx + 1
                                    };

                                if let Some(dragged_payload) =
                                    response.dnd_release_payload()
                                {
                                    // The user dropped onto this item.
                                    from = Some(dragged_payload);
                                    to = Some(Location {
                                        col: col_idx,
                                        row: insert_row_idx,
                                    });
                                }
                            }
                        }
                    });

                if let Some(dragged_payload) = dropped_payload {
                    // The user dropped onto the column, but not on any one item.
                    from = Some(dragged_payload);
                    to = Some(Location {
                        col: col_idx,
                        row: usize::MAX, // Inset last
                    });
                }
            }
        });

        if let (Some(from), Some(mut to)) = (from, to) {
            if from.col == to.col {
                // Dragging within the same column.
                // Adjust row index if we are re-ordering:
                to.row -= (from.row < to.row) as usize;
            }

            let item = self.columns[from.col].remove(from.row);

            let column = &mut self.columns[to.col];
            to.row = to.row.min(column.len());
            column.insert(to.row, item);
        }
    }
}

use egui::{Painter, Shape, pos2, widgets::Slider};
use std::f32::consts::TAU;

#[derive(PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
pub struct FractalClock {
    paused: bool,
    time: f64,
    zoom: f32,
    start_line_width: f32,
    depth: usize,
    length_factor: f32,
    luminance_factor: f32,
    width_factor: f32,
    line_count: usize,
}

impl Default for FractalClock {
    fn default() -> Self {
        Self {
            paused: false,
            time: 0.0,
            zoom: 0.25,
            start_line_width: 2.5,
            depth: 9,
            length_factor: 0.8,
            luminance_factor: 0.8,
            width_factor: 0.9,
            line_count: 0,
        }
    }
}

impl FractalClock {
    pub fn ui(&mut self, ui: &mut Ui, seconds_since_midnight: Option<f64>) {
        if !self.paused {
            self.time =
                seconds_since_midnight.unwrap_or_else(|| ui.input(|i| i.time));
            ui.ctx().request_repaint();
        }

        let painter = Painter::new(
            ui.ctx().clone(),
            ui.layer_id(),
            ui.available_rect_before_wrap(),
        );
        self.paint(&painter);
        // Make sure we allocate what we used (everything)
        ui.expand_to_include_rect(painter.clip_rect());

        Frame::popup(ui.style())
            .stroke(Stroke::NONE)
            .show(ui, |ui| {
                ui.set_max_width(270.0);
                CollapsingHeader::new("Settings")
                    .show(ui, |ui| self.options_ui(ui, seconds_since_midnight));
            });
    }

    fn options_ui(&mut self, ui: &mut Ui, seconds_since_midnight: Option<f64>) {
        if seconds_since_midnight.is_some() {
            ui.label(format!(
                "Local time: {:02}:{:02}:{:02}.{:03}",
                (self.time % (24.0 * 60.0 * 60.0) / 3600.0).floor(),
                (self.time % (60.0 * 60.0) / 60.0).floor(),
                (self.time % 60.0).floor(),
                (self.time % 1.0 * 100.0).floor()
            ));
        } else {
            ui.label("The fractal_clock clock is not showing the correct time");
        }
        ui.label(format!("Painted line count: {}", self.line_count));

        ui.checkbox(&mut self.paused, "Paused");
        ui.add(Slider::new(&mut self.zoom, 0.0..=1.0).text("zoom"));
        ui.add(
            Slider::new(&mut self.start_line_width, 0.0..=5.0)
                .text("Start line width"),
        );
        ui.add(Slider::new(&mut self.depth, 0..=14).text("depth"));
        ui.add(
            Slider::new(&mut self.length_factor, 0.0..=1.0)
                .text("length factor"),
        );
        ui.add(
            Slider::new(&mut self.luminance_factor, 0.0..=1.0)
                .text("luminance factor"),
        );
        ui.add(
            Slider::new(&mut self.width_factor, 0.0..=1.0).text("width factor"),
        );

        egui::reset_button(ui, self, "Reset");

        ui.hyperlink_to(
            "Inspired by a screensaver by Rob Mayoff",
            "http://www.dqd.com/~mayoff/programs/FractalClock/",
        );
    }

    fn paint(&mut self, painter: &Painter) {
        struct Hand {
            length: f32,
            angle: f32,
            vec: Vec2,
        }

        impl Hand {
            fn from_length_angle(length: f32, angle: f32) -> Self {
                Self {
                    length,
                    angle,
                    vec: length * Vec2::angled(angle),
                }
            }
        }

        let angle_from_period = |period| {
            TAU * (self.time.rem_euclid(period) / period) as f32 - TAU / 4.0
        };

        let hands = [
            // Second hand:
            Hand::from_length_angle(
                self.length_factor,
                angle_from_period(60.0),
            ),
            // Minute hand:
            Hand::from_length_angle(
                self.length_factor,
                angle_from_period(60.0 * 60.0),
            ),
            // Hour hand:
            Hand::from_length_angle(0.5, angle_from_period(12.0 * 60.0 * 60.0)),
        ];

        let mut shapes: Vec<Shape> = Vec::new();

        let rect = painter.clip_rect();
        let to_screen = emath::RectTransform::from_to(
            Rect::from_center_size(
                Pos2::ZERO,
                rect.square_proportions() / self.zoom,
            ),
            rect,
        );

        let mut paint_line = |points: [Pos2; 2], color: Color32, width: f32| {
            let line = [to_screen * points[0], to_screen * points[1]];

            // culling
            if rect.intersects(Rect::from_two_pos(line[0], line[1])) {
                shapes.push(Shape::line_segment(line, (width, color)));
            }
        };

        let hand_rotations = [
            hands[0].angle - hands[2].angle + TAU / 2.0,
            hands[1].angle - hands[2].angle + TAU / 2.0,
        ];

        let hand_rotors = [
            hands[0].length * emath::Rot2::from_angle(hand_rotations[0]),
            hands[1].length * emath::Rot2::from_angle(hand_rotations[1]),
        ];

        #[derive(Clone, Copy)]
        struct Node {
            pos: Pos2,
            dir: Vec2,
        }

        let mut nodes = Vec::new();

        let mut width = self.start_line_width;

        for (i, hand) in hands.iter().enumerate() {
            let center = pos2(0.0, 0.0);
            let end = center + hand.vec;
            paint_line(
                [center, end],
                Color32::from_additive_luminance(255),
                width,
            );
            if i < 2 {
                nodes.push(Node {
                    pos: end,
                    dir: hand.vec,
                });
            }
        }

        let mut luminance = 0.7; // Start dimmer than main hands

        let mut new_nodes = Vec::new();
        for _ in 0..self.depth {
            new_nodes.clear();
            new_nodes.reserve(nodes.len() * 2);

            luminance *= self.luminance_factor;
            width *= self.width_factor;

            let luminance_u8 = (255.0 * luminance).round() as u8;
            if luminance_u8 == 0 {
                break;
            }

            for &rotor in &hand_rotors {
                for a in &nodes {
                    let new_dir = rotor * a.dir;
                    let b = Node {
                        pos: a.pos + new_dir,
                        dir: new_dir,
                    };
                    paint_line(
                        [a.pos, b.pos],
                        Color32::from_additive_luminance(luminance_u8),
                        width,
                    );
                    new_nodes.push(b);
                }
            }

            std::mem::swap(&mut nodes, &mut new_nodes);
        }
        self.line_count = shapes.len();
        painter.extend(shapes);
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
    let mut show_plaintext =
        ui.data_mut(|d| d.get_temp::<bool>(state_id).unwrap_or(false));

    // Process ui, change a local copy of the state
    // We want TextEdit to fill entire space, and have button after that, so in that case we can
    // change direction to right_to_left.
    let result = ui.with_layout(
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
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
        },
    );

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

#[derive(Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
struct Tree(Vec<Tree>);

impl Tree {
    pub fn demo() -> Self {
        Self(vec![
            Self(vec![Self::default(); 4]),
            Self(vec![Self(vec![Self::default(); 2]); 3]),
        ])
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Action {
        self.ui_impl(ui, 0, "root")
    }
}

impl Tree {
    fn ui_impl(&mut self, ui: &mut Ui, depth: usize, name: &str) -> Action {
        CollapsingHeader::new(name)
            .default_open(depth < 1)
            .show(ui, |ui| self.children_ui(ui, depth))
            .body_returned
            .unwrap_or(Action::Keep)
    }

    fn children_ui(&mut self, ui: &mut Ui, depth: usize) -> Action {
        if depth > 0
            && ui
                .button(
                    RichText::new("delete").color(ui.visuals().warn_fg_color),
                )
                .clicked()
        {
            return Action::Delete;
        }

        self.0 = std::mem::take(self)
            .0
            .into_iter()
            .enumerate()
            .filter_map(|(i, mut tree)| {
                if tree.ui_impl(ui, depth + 1, &format!("child #{i}"))
                    == Action::Keep
                {
                    Some(tree)
                } else {
                    None
                }
            })
            .collect();

        if ui.button("+").clicked() {
            self.0.push(Self::default());
        }

        Action::Keep
    }
}
use egui::{CollapsingHeader, Image, UserData, ViewportCommand, Widget as _};

/// Showcase [`ViewportCommand::Screenshot`].
#[derive(PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
pub struct Screenshot {
    #[dbg(skip)]
    image: Option<(Arc<egui::ColorImage>, egui::TextureHandle)>,
    continuous: bool,
}

impl Screenshot {
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.set_width(300.0);

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label("This demo showcases how to take screenshots via ");
            ui.code("ViewportCommand::Screenshot");
            ui.label(".");
        });

        ui.horizontal_top(|ui| {
            let capture = ui.button("📷 Take Screenshot").clicked();
            ui.checkbox(&mut self.continuous, "Capture continuously");
            if capture || self.continuous {
                ui.ctx().send_viewport_cmd(ViewportCommand::Screenshot(
                    UserData::default(),
                ));
            }
        });

        let image = ui.ctx().input(|i| {
            i.events
                .iter()
                .filter_map(|e| {
                    if let egui::Event::Screenshot { image, .. } = e {
                        Some(image.clone())
                    } else {
                        None
                    }
                })
                .next_back()
        });

        if let Some(image) = image {
            self.image = Some((
                image.clone(),
                ui.ctx().load_texture(
                    "screenshot_demo",
                    image,
                    Default::default(),
                ),
            ));
        }

        if let Some((_, texture)) = &self.image {
            Image::new(texture).shrink_to_fit().ui(ui);
        } else {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.set_height(100.0);
                ui.centered_and_justified(|ui| {
                    ui.label("No screenshot taken yet.");
                });
            });
        }
    }
}

use egui::{
    Context as EguiContext, Frame, Pos2, Rect, Sense, Ui, Window, emath, vec2,
};
impl Painting {
    pub fn ui_control(&mut self, ui: &mut egui::Ui) -> egui::Response {
        ui.horizontal(|ui| {
            ui.label("Stroke:");
            ui.add(&mut self.stroke);
            ui.separator();
            if ui.button("Clear Painting").clicked() {
                self.lines.clear();
            }
        })
        .response
    }

    pub fn ui_content(&mut self, ui: &mut Ui) -> egui::Response {
        let (mut response, painter) =
            ui.allocate_painter(ui.available_size_before_wrap(), Sense::drag());

        let to_screen = emath::RectTransform::from_to(
            Rect::from_min_size(Pos2::ZERO, response.rect.square_proportions()),
            response.rect,
        );
        let from_screen = to_screen.inverse();

        if self.lines.is_empty() {
            self.lines.push(vec![]);
        }

        let current_line = self.lines.last_mut().unwrap();

        if let Some(pointer_pos) = response.interact_pointer_pos() {
            let canvas_pos = from_screen * pointer_pos;
            if current_line.last() != Some(&canvas_pos) {
                current_line.push(canvas_pos);
                response.mark_changed();
            }
        } else if !current_line.is_empty() {
            self.lines.push(vec![]);
            response.mark_changed();
        }

        let shapes =
            self.lines
                .iter()
                .filter(|line| line.len() >= 2)
                .map(|line| {
                    let points: Vec<Pos2> =
                        line.iter().map(|p| to_screen * *p).collect();
                    egui::Shape::line(points, self.stroke)
                });

        painter.extend(shapes);

        response
    }
    fn name(&self) -> &'static str {
        "🖊 Painting"
    }
    fn show(&mut self, ctx: &EguiContext, open: &mut bool) {
        Window::new(self.name())
            .open(open)
            .default_size(vec2(512.0, 512.0))
            .vscroll(false)
            .show(ctx, |ui| self.ui(ui));
    }
    fn ui(&mut self, ui: &mut Ui) {
        self.ui_control(ui);
        ui.label("Paint with your mouse/touch!");
        Frame::canvas(ui.style()).show(ui, |ui| {
            self.ui_content(ui);
        });
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
pub struct Painting {
    /// in 0-1 normalized coordinates
    lines: Vec<Vec<Pos2>>,
    stroke: Stroke,
}

impl Default for Painting {
    fn default() -> Self {
        Self {
            lines: Default::default(),
            stroke: Stroke::new(1.0, Color32::from_rgb(25, 200, 100)),
        }
    }
}

use egui::{Button, util::undoer::Undoer};

#[derive(PartialEq, Eq, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
pub struct State {
    pub toggle_value: bool,
    pub text: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            toggle_value: Default::default(),
            text: "Text with undo/redo".to_owned(),
        }
    }
}

#[derive(Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct UndoRedoDemo {
    pub state: State,
    pub undoer: Undoer<State>,
}

impl UndoRedoDemo {
    fn name(&self) -> &'static str {
        "⟲ Undo Redo"
    }
    fn show(&mut self, ctx: &egui::Context, open: &mut bool) {
        egui::Window::new(self.name())
            .open(open)
            .resizable(false)
            .show(ctx, |ui| {
                self.ui(ui);
            });
    }
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.state.toggle_value, "Checkbox with undo/redo");
        ui.text_edit_singleline(&mut self.state.text);

        ui.separator();

        let can_undo = self.undoer.has_undo(&self.state);
        let can_redo = self.undoer.has_redo(&self.state);

        ui.horizontal(|ui| {
            let undo =
                ui.add_enabled(can_undo, Button::new("⟲ Undo")).clicked();
            let redo =
                ui.add_enabled(can_redo, Button::new("⟳ Redo")).clicked();

            if undo && let Some(undo_text) = self.undoer.undo(&self.state) {
                self.state = undo_text.clone();
            }
            if redo && let Some(redo_text) = self.undoer.redo(&self.state) {
                self.state = redo_text.clone();
            }
        });

        self.undoer
            .feed_state(ui.ctx().input(|input| input.time), &self.state);
    }
}

#[derive(PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
pub struct TextEditDemo {
    pub text: String,
}

impl Default for TextEditDemo {
    fn default() -> Self {
        Self {
            text: "Edit this text".to_owned(),
        }
    }
}

impl TextEditDemo {
    fn name(&self) -> &'static str {
        "🖹 TextEdit"
    }

    fn show(&mut self, ctx: &egui::Context, open: &mut bool) {
        egui::Window::new(self.name())
            .open(open)
            .resizable(false)
            .show(ctx, |ui| {
                self.ui(ui);
            });
    }
    fn ui(&mut self, ui: &mut egui::Ui) {
        let Self { text } = self;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label("Advanced usage of ");
            ui.code("TextEdit");
            ui.label(".");
        });

        let output = egui::TextEdit::multiline(text)
            .hint_text("Type something!")
            .show(ui);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label("Selected text: ");
            if let Some(text_cursor_range) = output.cursor_range {
                let selected_text = text_cursor_range.slice_str(text);
                ui.code(selected_text);
            }
        });

        let anything_selected =
            output.cursor_range.is_some_and(|cursor| !cursor.is_empty());

        ui.add_enabled(
            anything_selected,
            egui::Label::new("Press ctrl+Y to toggle the case of selected text (cmd+Y on Mac)"),
        );

        if ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y)
        }) && let Some(text_cursor_range) = output.cursor_range
        {
            use egui::TextBuffer as _;
            let selected_chars = text_cursor_range.as_sorted_char_range();
            let selected_text = text.char_range(selected_chars.clone());
            let upper_case = selected_text.to_uppercase();
            let new_text = if selected_text == upper_case {
                selected_text.to_lowercase()
            } else {
                upper_case
            };
            text.delete_char_range(selected_chars.clone());
            text.insert_text(&new_text, selected_chars.start);
        }

        ui.horizontal(|ui| {
            ui.label("Move cursor to the:");

            if ui.button("start").clicked() {
                let text_edit_id = output.response.id;
                if let Some(mut state) =
                    egui::TextEdit::load_state(ui.ctx(), text_edit_id)
                {
                    let ccursor = egui::text::CCursor::new(0);
                    state.cursor.set_char_range(Some(
                        egui::text::CCursorRange::one(ccursor),
                    ));
                    state.store(ui.ctx(), text_edit_id);
                    ui.ctx().memory_mut(|mem| mem.request_focus(text_edit_id)); // give focus back to the [`TextEdit`].
                }
            }

            if ui.button("end").clicked() {
                let text_edit_id = output.response.id;
                if let Some(mut state) =
                    egui::TextEdit::load_state(ui.ctx(), text_edit_id)
                {
                    let ccursor =
                        egui::text::CCursor::new(text.chars().count());
                    state.cursor.set_char_range(Some(
                        egui::text::CCursorRange::one(ccursor),
                    ));
                    state.store(ui.ctx(), text_edit_id);
                    ui.ctx().memory_mut(|mem| mem.request_focus(text_edit_id)); // give focus back to the [`TextEdit`].
                }
            }
        });
    }
}

/////////////////////////////////////NOTE RAW WIDGETS///////////////////////////////////XXX

use crate::trade::HistTrade as HistTradeRunner;
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
struct ManualOrders {
    man_orders: Option<HistTrade>,
    active_orders: Option<Vec<Order>>,

    hist_trade_runner:HistTradeRunner,

    search_string: String,
    quant: Quant,
    order: Order,
    new_order: Order,
    scalar_set: bool,
    scalar: f64,
    buy: bool,
    price_string: String,
    stop_price_string: String,
    orders: HashMap<i32, (Order,bool)>,
    asset1: f64,
    asset2: f64,
    last_id: i32,
    asset1_name: String,
    asset2_name: String,
    last_price_buffer: Vec<f64>,
    last_price_buffer_size: usize,
    last_price_s: f64,

    plot_extras:Option<PlotExtras>
}

use std::collections::HashMap;

//TODO find a way to group GUI elements together. The horror...
impl Default for ManualOrders {
    fn default() -> Self {
        Self {
            man_orders: None,
            active_orders: None,

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

            hist_trade_runner:HistTradeRunner::default(),

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

            plot_extras:None
        }
    }
}
use egui::{FontId, RichText};


#[instrument(level="debug")]
fn link_hline_orders(orders:&HashMap<i32,(Order,bool)>,hlines: &mut Vec<HLine>){
    let mut hl:Vec<HLine>=orders.iter().map(|(_,(order,active))|{
        HlineType::hline_order(order,*active)
    }).collect();
    tracing::debug!["Hlines (link_hline_orders):{:?}",&hl];
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
    fn check_order_hist(&self, order:&Order, hist_trade:&mut HistTrade)->Result<()>{
        //todo!()
        Ok(())
    }
    fn check_order_price(&self, order:&Order, last_price:&f64)->Result<()>{
        Ok(())
        //todo!()
    }
    fn sync(&mut self, hist_trade:Option<&HistTrade>){
        todo!()

    }
    fn show(
        mut man_orders: &mut ManualOrders,
        last_price: &f64,
        mut hist_trade: Option<&mut HistTrade>,
        cli_chan: watch::Sender<ClientInstruct>,
        ui: &mut egui::Ui,
        hlines:Option<&mut Vec<HLine>>
    ) -> Result<()> {
        match hlines{
            Some(hline_ref)=>link_hline_orders(&man_orders.orders,hline_ref),
            None=>{
                tracing::debug!["Hlines not available!"];
            }
        };
        //man_orders.sync(hist_trade.as_deref());
        egui::Grid::new("Man order assets:")
            .striped(true)
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(format![
                            "{}:{}",
                            man_orders.asset1_name, man_orders.asset1
                        ])
                        .color(Color32::GREEN),
                    );
                    ui.end_row();
                });
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(format![
                            "{}:{}",
                            man_orders.asset2_name, man_orders.asset2
                        ])
                        .color(Color32::RED),
                    );
                    ui.end_row();
                });
                ui.vertical(|ui| {
                    if *last_price != man_orders.last_price_s {
                        man_orders.last_price_s = *last_price;
                        man_orders.last_price_buffer.push(last_price.clone());
                        let n = man_orders.last_price_buffer.len();
                        if n < man_orders.last_price_buffer_size {
                            ui.label(
                                RichText::new(format![
                                    "Last price:{}",
                                    last_price
                                ])
                                .color(Color32::WHITE),
                            );
                        } else {
                            let sum: f64 =
                                man_orders.last_price_buffer.iter().sum();
                            if sum / (n as f64) <= *last_price {
                                ui.label(
                                    RichText::new(format![
                                        "Last price:{}",
                                        last_price
                                    ])
                                    .color(Color32::GREEN),
                                );
                            } else {
                                ui.label(
                                    RichText::new(format![
                                        "Last price:{}",
                                        last_price
                                    ])
                                    .color(Color32::RED),
                                );
                            }
                        }
                    }
                    ui.end_row();
                });
                /*
                 */
            });

        ui.end_row();
        let pp: f64 = man_orders.price_string.parse()?;
        let sp: f64 = man_orders.price_string.parse()?;

        egui::Grid::new("parent grid").striped(true).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.end_row();

                ui.end_row();
                let mut dummy: Option<f64> = None;

                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut man_orders.quant,
                        Quant::Q25,
                        "25%",
                    );
                    ui.selectable_value(
                        &mut man_orders.quant,
                        Quant::Q50,
                        "50%",
                    );
                    ui.selectable_value(
                        &mut man_orders.quant,
                        Quant::Q75,
                        "75%",
                    );
                    ui.selectable_value(
                        &mut man_orders.quant,
                        Quant::Q100,
                        "100%",
                    );
                    ui.selectable_value(&mut dummy, None, "");
                });
                ui.end_row();

                ui.add(
                    egui::Slider::new(&mut man_orders.scalar, 0.0..=100.0)
                        .suffix(format!("%")),
                );
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
                            egui::TextEdit::singleline(
                                &mut man_orders.price_string,
                            )
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
                            egui::TextEdit::singleline(
                                &mut man_orders.price_string,
                            )
                            .hint_text("Enter the limit price"),
                        );
                        ui.label("Enter the stop price:");
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut man_orders.stop_price_string,
                            )
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
                            egui::TextEdit::singleline(
                                &mut man_orders.stop_price_string,
                            )
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
                    let res:Result<()>=match hist_trade{
                        Some(mut hist_trade)=>{
                            let res1=man_orders.check_order_price(&o, &last_price);
                            match res1{
                                Ok(_)=>{
                                    let res=man_orders.check_order_hist(&o, &mut hist_trade);
                                        match res{
                                            Ok(_)=>{
                                                hist_trade.place_order(&o);
                                                Ok(())
                                            },
                                            Err(e)=>Err(e),
                                        }

                                },
                                Err(e)=>Err(e),
                            }
                        }
                        None=>{
                            todo!();

                        }
                    };
                    match res{
                        Ok(_)=>{
                            man_orders.last_id += 1;
                            let oid = man_orders.last_id.clone();
                            man_orders.orders.insert(oid, (o, false));
                            //NOTE allow only one order for now...
                        }
                        Err(e)=>{
                            tracing::error!["Invalid order:{:?}, error:{}",&o,e];
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
                    .cell_layout(egui::Layout::left_to_right(
                        egui::Align::Center,
                    ))
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
                        for (id,(order,active)) in orders.iter() {
                            let row_height = 18.0;
                            body.row(row_height, |mut row| {
                                row.col(|ui| {
                                    ui.label(format!["{}", id]);
                                });
                                row.col(|ui| {
                                    ui.label(format![
                                        "{}",
                                        order.get_qnt() * man_orders.asset1
                                    ]);
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

//NOTE////////////////////////////////// NON RELEASE FEATURES /////////////////////////////NOTE//
fn test_chart(ui: &mut egui::Ui) {
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

    let plot = Plot::new("candlestick chart")
        .legend(Legend::default())
        .x_axis_formatter(x_format_1d)
        .x_grid_spacer(uniform_grid_spacer(grid_spacer_1d))
        .view_aspect(2.0)
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
    search_date: String,
    search_load_string: String,
    intv: Intv,
    picked_date: chrono::NaiveDate,
    picked_date_end: chrono::NaiveDate,
    lines:Vec<HLine>,
}

impl Default for LivePlot {
    fn default() -> Self {
        Self {
            kline_plot: KlinePlot::default(),
            live_asset_data: Arc::new(Mutex::new(AssetData::new(666))),

            search_string: "".to_string(),
            search_date: "".to_string(),
            search_load_string: "".to_string(),
            intv: Intv::Min15,
            picked_date: chrono::NaiveDate::from_ymd(2024, 10, 1),
            picked_date_end: chrono::NaiveDate::from_ymd(2025, 10, 10),
            lines:vec![]
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
        collect_data: &HashMap<String,SymbolOutput>,
        ui: &mut egui::Ui,
    ) {
        live_plot.kline_plot.show(ui, plot_extras);

        ui.end_row();
        egui::Grid::new("Hplot order assets:")
            .striped(true)
            .show(ui, |ui| {
                egui::ComboBox::from_label("Interval")
                    .selected_text(format!("{}", live_plot.intv.to_str()))
                    .show_ui(ui, |ui| {
                        for i in Intv::iter() {
                            ui.selectable_value(
                                &mut live_plot.intv,
                                i,
                                i.to_str(),
                            );
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut live_plot.search_string)
                        .hint_text("Enter date or pick period"),
                );
                ui.add(egui_extras::DatePickerButton::new(
                    &mut live_plot.picked_date,
                ).id_salt("live_start"));
                ui.add(egui_extras::DatePickerButton::new(
                    &mut live_plot.picked_date_end,
                ).id_salt("live_end"));
                let search = ui.add(
                    egui::TextEdit::singleline(&mut live_plot.search_string)
                        .hint_text("Search for loaded asset"),
                );
                if ui.button("Show").clicked() {
                    let s = &live_plot.search_string.clone();
                    let ad_lock=live_plot.live_asset_data.lock().expect("Posoned mutex! - Live asset data");
                    let ad = ad_lock;
                    let i = live_plot.intv;
                    let res =
                        live_plot.kline_plot.live_from_ad(&ad, s, i, 1_000, None);
                }
                let search_load = ui.add(
                    egui::TextEdit::singleline(
                        &mut live_plot.search_load_string,
                    )
                    .hint_text("Asset to Load"),
                );
                if ui.button("Load Asset").clicked() {
                    let msg = ClientInstruct::SendSQLInstructs(
                        SQLInstructs::LoadHistData {
                            symbol: live_plot.search_load_string.clone(),
                        },
                    );
                    cli_chan.send(msg);
                }
            });
    }
}


#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Copy, Clone)]
#[derive(Dbg)]
struct HistExtras{
    last_price:f64,
}
impl Default for HistExtras{
    fn default()->Self{
        Self{
            last_price:0.0
        }
    }
}


#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[derive(Dbg)]
struct HistPlot {
    hist_asset_data: Arc<Mutex<AssetData>>,
    kline_plot: KlinePlot,
    search_string: String,
    search_date: String,
    search_load_string: String,
    intv: Intv,
    picked_date: chrono::NaiveDate,
    picked_date_end: chrono::NaiveDate,
    hist_extras:Option<HistExtras>,
    hist_trade:HistTrade,
}

impl Default for HistPlot {
    fn default() -> Self {
        Self {
            kline_plot: KlinePlot::default(),
            hist_asset_data: Arc::new(Mutex::new(AssetData::new(666))),

            search_string: "".to_string(),
            search_date: "".to_string(),
            search_load_string: "".to_string(),
            intv: Intv::Min15,
            picked_date: chrono::NaiveDate::from_ymd(2024, 10, 1),
            picked_date_end: chrono::NaiveDate::from_ymd(2025, 10, 10),
            hist_extras:None,
            hist_trade:HistTrade::default(),
        }
    }
}

enum LineStyle{
    Solid(f32),
    Dotted(f32)
}

enum LineState{
    ActiveColor(Color32),
    InactiveColor(Color32)
}

enum HlineType{
    BuyOrder((LineState,LineStyle)),
    SellOrder((LineState,LineStyle)),
    LastPrice((LineStyle,Color32)),
}

const buy_active:LineState=LineState::ActiveColor(Color32::GREEN);
const buy_inactive:LineState=LineState::InactiveColor(Color32::GREEN);
const buy_style:LineStyle=LineStyle::Solid(0.5);


const sell_active:LineState=LineState::ActiveColor(Color32::RED);
const sell_inactive:LineState=LineState::InactiveColor(Color32::RED);
const sell_style:LineStyle=LineStyle::Solid(0.5);

const last_price_color:Color32=Color32::YELLOW;
const last_price_line:LineStyle=LineStyle::Dotted(0.5);


impl HlineType{
    fn hline_order(o:&Order,active:bool)->HLine{
        let side=o.get_side();
        let price=o.get_price();
        if side ==true{
            if active == true{
                return HlineType::BuyOrder((buy_active,buy_style)).to_hline(price);
            }else{
                return HlineType::BuyOrder((buy_inactive,buy_style)).to_hline(price);
            }
        }else{
            if active == true{
                return HlineType::SellOrder((sell_active,sell_style)).to_hline(price);
            }else{
                return HlineType::SellOrder((sell_inactive,sell_style)).to_hline(price);
            }
        }
    }
    fn to_hline(&self,value:&f64)->HLine{
        match &self{
            HlineType::BuyOrder((ba,bs))=>{
                let color=match ba{
                    LineState::ActiveColor(color)=>color,
                    LineState::InactiveColor(color)=>color,
                };
                let width=match bs{
                    LineStyle::Solid(width)=>width,
                    LineStyle::Dotted(width)=>width,
                };
                let s = Stroke::new(width.clone(), color.clone());
                HLine::new("Buy order", value.clone()).stroke(s)
            }
            HlineType::SellOrder((si,ss))=>{
                let color=match si{
                    LineState::ActiveColor(color)=>color,
                    LineState::InactiveColor(color)=>color,
                };
                let width=match ss{
                    LineStyle::Solid(width)=>width,
                    LineStyle::Dotted(width)=>width,
                };
                let s = Stroke::new(width.clone(), color.clone());
                HLine::new("Sell order", value.clone()).stroke(s)
            }
            HlineType::LastPrice((l,color))=>{
                let width=match l{
                    LineStyle::Solid(width)=>width,
                    LineStyle::Dotted(width)=>width,
                };
                let s = Stroke::new(width.clone(), color.clone());
                HLine::new("Last price", value.clone()).stroke(s)
            }
        }
    }
}




impl HistPlot {
    fn create_hlines(&self, order_price:&[(Order,bool)])->Vec<HLine>{
        order_price.iter().map(|(order,active)|{
            HlineType::hline_order(order,*active)
        }).collect()
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
        ui: &mut egui::Ui,
    ) {
        hist_plot.kline_plot.show(ui, plot_extras);

        ui.end_row();
        egui::Grid::new("Hplot order assets:")
            .striped(true)
            .show(ui, |ui| {
                egui::ComboBox::from_label("Interval")
                    .selected_text(format!("{}", hist_plot.intv.to_str()))
                    .show_ui(ui, |ui| {
                        for i in Intv::iter() {
                            ui.selectable_value(
                                &mut hist_plot.intv,
                                i,
                                i.to_str(),
                            );
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut hist_plot.search_string)
                        .hint_text("Enter date or pick period"),
                );
                ui.add(egui_extras::DatePickerButton::new(
                    &mut hist_plot.picked_date,
                ).id_salt("hist_start"));
                ui.add(egui_extras::DatePickerButton::new(
                    &mut hist_plot.picked_date_end,
                ).id_salt("hist_end"));
                let search = ui.add(
                    egui::TextEdit::singleline(&mut hist_plot.search_string)
                        .hint_text("Search for loaded asset"),
                );
                if ui.button("Show").clicked() {
                    let s = &hist_plot.search_string.clone();
                    let ad = hist_plot.hist_asset_data.lock().expect("Posoned mutex! - Hist asset data");
                    let i = hist_plot.intv;
                    let time=chrono::NaiveTime::from_hms_milli_opt(0, 0, 0, 0).expect("Really?...");
                    let res =
                        hist_plot.kline_plot.live_from_ad(&ad, s, i, 1_000, None
                            //hist_plot.picked_date.and_time(time), hist_plot.picked_date_end.and_time(time))
                            );
                }
            });
        egui::Grid::new("HistLoad")
            .striped(true)
            .show(ui, |ui| {
                let search_load = ui.add(
                    egui::TextEdit::singleline(
                        &mut hist_plot.search_load_string,
                    )
                    .hint_text("Asset to Load"),
                );
                if ui.button("Load Asset").clicked() {
                    let msg = ClientInstruct::SendSQLInstructs(
                        SQLInstructs::LoadHistData {
                            symbol: hist_plot.search_load_string.clone(),
                        },
                    );
                    cli_chan.send(msg);
                }
            });
        egui::Grid::new("Hplot forwards:").show(ui, |ui| {
            if ui.button("Forward").clicked() {
                todo!()
            }
            if ui.button("Backward").clicked() {
                todo!()
            }
            if ui.button("N Wicks").clicked() {
                todo!()
            }
        });
    }
}

use tracing::instrument;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample() {
        assert!(true);
    }
}
