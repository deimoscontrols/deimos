use plotly::{
    HeatMap, Layout, Plot, Scatter,
    common::{ColorScale, ColorScalePalette, DashType, Line, Marker, MarkerSymbol, Mode, Title},
    layout::{Axis, AxisConstrain, AxisType},
};

/// One line-oriented Plotly trace.
#[derive(Clone)]
pub struct LineSeries {
    /// Legend label for the trace.
    pub name: String,
    /// X-axis samples.
    pub x: Vec<f64>,
    /// Y-axis samples.
    pub y: Vec<f64>,
    /// Plotly mode for the trace.
    pub mode: Mode,
    /// Overall trace opacity.
    pub opacity: f64,
    /// Line width in CSS pixels.
    pub line_width: f64,
    /// Optional explicit dash style override.
    pub dash: Option<DashType>,
    /// Optional explicit marker symbol override.
    pub marker_symbol: Option<MarkerSymbol>,
    /// Optional explicit marker size override in CSS pixels.
    pub marker_size: Option<usize>,
}

impl LineSeries {
    /// Creates a line-only series.
    #[must_use]
    pub fn lines(name: impl Into<String>, x: Vec<f64>, y: Vec<f64>) -> Self {
        Self {
            name: name.into(),
            x,
            y,
            mode: Mode::Lines,
            opacity: 1.0,
            line_width: 2.0,
            dash: None,
            marker_symbol: None,
            marker_size: None,
        }
    }

    /// Creates a line-and-marker series.
    #[must_use]
    pub fn lines_markers(name: impl Into<String>, x: Vec<f64>, y: Vec<f64>) -> Self {
        Self {
            name: name.into(),
            x,
            y,
            mode: Mode::LinesMarkers,
            opacity: 1.0,
            line_width: 2.0,
            dash: None,
            marker_symbol: None,
            marker_size: None,
        }
    }

    /// Creates a marker-only series.
    #[must_use]
    pub fn markers(name: impl Into<String>, x: Vec<f64>, y: Vec<f64>) -> Self {
        Self {
            name: name.into(),
            x,
            y,
            mode: Mode::Markers,
            opacity: 1.0,
            line_width: 2.0,
            dash: None,
            marker_symbol: None,
            marker_size: None,
        }
    }

    /// Overrides the trace opacity.
    #[must_use]
    pub fn with_opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity;
        self
    }

    /// Overrides the line width.
    #[must_use]
    pub fn with_line_width(mut self, line_width: f64) -> Self {
        self.line_width = line_width;
        self
    }

    /// Overrides the dash pattern.
    #[must_use]
    pub fn with_dash(mut self, dash: DashType) -> Self {
        self.dash = Some(dash);
        self
    }

    /// Overrides the marker symbol.
    #[must_use]
    pub fn with_marker_symbol(mut self, marker_symbol: MarkerSymbol) -> Self {
        self.marker_symbol = Some(marker_symbol);
        self
    }

    /// Overrides the marker size.
    #[must_use]
    pub fn with_marker_size(mut self, marker_size: usize) -> Self {
        self.marker_size = Some(marker_size);
        self
    }
}

/// Builds a multi-trace line plot with optional logarithmic x-axis scaling.
#[must_use]
pub fn build_line_plot(
    title: &str,
    x_label: &str,
    y_label: &str,
    log_x: bool,
    series: Vec<LineSeries>,
) -> Plot {
    let mut plot = Plot::new();
    for (index, trace) in series.into_iter().enumerate() {
        let line_style = Line::new()
            .color("#000000")
            .dash(trace.dash.unwrap_or_else(|| dash_style(index)))
            .width(trace.line_width);
        let marker_size = trace.marker_size.unwrap_or(11);
        let marker_style = match trace.marker_symbol {
            Some(symbol) => Marker::new()
                .color("#000000")
                .symbol(symbol)
                .size(marker_size),
            None => Marker::new().color("#000000"),
        };
        plot.add_trace(
            Scatter::new(trace.x, trace.y)
                .mode(trace.mode)
                .name(trace.name)
                .opacity(trace.opacity)
                .line(line_style)
                .marker(marker_style),
        );
    }

    let mut x_axis = Axis::new().title(Title::with_text(x_label));
    if log_x {
        x_axis = x_axis.type_(AxisType::Log);
    }

    plot.set_layout(
        Layout::new()
            .title(Title::with_text(title))
            .x_axis(x_axis)
            .y_axis(Axis::new().title(Title::with_text(y_label))),
    );
    plot
}

/// Builds a plot on the complex plane with symmetric real and imaginary axes.
#[must_use]
pub fn build_complex_plane_plot(title: &str, series: Vec<LineSeries>) -> Plot {
    let (x_radius, y_radius) = symmetric_complex_plane_half_ranges(&series);
    let mut plot = build_line_plot(title, "real", "imaginary", false, series);

    plot.set_layout(
        Layout::new()
            .title(Title::with_text(title))
            .x_axis(
                Axis::new()
                    .title(Title::with_text("real"))
                    .range(vec![-x_radius, x_radius])
                    .constrain(AxisConstrain::Range),
            )
            .y_axis(
                Axis::new()
                    .title(Title::with_text("imaginary"))
                    .range(vec![-y_radius, y_radius])
                    .constrain(AxisConstrain::Range),
            ),
    );
    plot
}

/// Builds a heatmap view of a dense matrix.
#[must_use]
pub fn build_matrix_heatmap_plot(
    title: &str,
    values: Vec<Vec<f64>>,
    symmetric_about_zero: bool,
) -> Plot {
    let nrows = values.len();
    let ncols = values.first().map_or(0, Vec::len);
    let x = (0..ncols).map(|idx| (idx + 1) as f64).collect::<Vec<_>>();
    let y = (0..nrows).map(|idx| (idx + 1) as f64).collect::<Vec<_>>();

    let (zmin, zmax, zmid) = if symmetric_about_zero {
        let max_abs = values
            .iter()
            .flat_map(|row| row.iter())
            .map(|value| value.abs())
            .fold(0.0_f64, f64::max)
            .max(1.0e-12);
        (-max_abs, max_abs, Some(0.0))
    } else {
        let min_value = values
            .iter()
            .flat_map(|row| row.iter())
            .copied()
            .fold(f64::INFINITY, f64::min);
        let max_value = values
            .iter()
            .flat_map(|row| row.iter())
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let fallback = if min_value.is_finite() && max_value.is_finite() {
            None
        } else {
            Some((0.0, 1.0))
        };
        match fallback {
            Some((zmin, zmax)) => (zmin, zmax, None),
            None => (min_value, max_value.max(min_value + 1.0e-12), None),
        }
    };

    let colorscale = if symmetric_about_zero {
        ColorScale::Palette(ColorScalePalette::RdBu)
    } else {
        ColorScale::Palette(ColorScalePalette::Greys)
    };

    let mut plot = Plot::new();
    let mut trace = HeatMap::new(x, y, values)
        .color_scale(colorscale)
        .zmin(zmin)
        .zmax(zmax)
        .x_gap(1)
        .y_gap(1);
    if let Some(zmid) = zmid {
        trace = trace.zmid(zmid);
    }
    plot.add_trace(trace);
    plot.set_layout(
        Layout::new()
            .title(Title::with_text(title))
            .x_axis(
                Axis::new()
                    .title(Title::with_text("column"))
                    .range(vec![0.5, (ncols.max(1) as f64) + 0.5])
                    .show_grid(false)
                    .zero_line(false)
                    .show_line(false),
            )
            .y_axis(
                Axis::new()
                    .title(Title::with_text("row"))
                    .range(vec![(nrows.max(1) as f64) + 0.5, 0.5])
                    .show_grid(false)
                    .zero_line(false)
                    .show_line(false)
                    .constrain(AxisConstrain::Range)
                    .scale_anchor("x")
                    .scale_ratio(1.0),
            ),
    );
    plot
}

/// Builds a spy-style sparse pattern plot from row/column nonzero locations.
#[must_use]
pub fn build_sparse_pattern_plot(
    title: &str,
    nrows: usize,
    ncols: usize,
    columns: Vec<f64>,
    rows: Vec<f64>,
) -> Plot {
    let mut plot = Plot::new();
    plot.add_trace(
        Scatter::new(columns, rows).mode(Mode::Markers).marker(
            Marker::new()
                .color("#000000")
                .size(4)
                .symbol(MarkerSymbol::Square),
        ),
    );
    plot.set_layout(
        Layout::new()
            .title(Title::with_text(title))
            .x_axis(
                Axis::new()
                    .title(Title::with_text("column"))
                    .range(vec![0.5, (ncols.max(1) as f64) + 0.5])
                    .show_grid(false)
                    .zero_line(false)
                    .show_line(false)
                    .constrain(AxisConstrain::Range),
            )
            .y_axis(
                Axis::new()
                    .title(Title::with_text("row"))
                    .range(vec![(nrows.max(1) as f64) + 0.5, 0.5])
                    .show_grid(false)
                    .zero_line(false)
                    .show_line(false)
                    .constrain(AxisConstrain::Range)
                    .scale_anchor("x")
                    .scale_ratio(1.0),
            ),
    );
    plot
}

/// Samples a dense matrix into a row-major nested vector for plotting.
#[must_use]
pub fn matrix_grid_from_fn(
    nrows: usize,
    ncols: usize,
    mut sample: impl FnMut(usize, usize) -> f64,
) -> Vec<Vec<f64>> {
    (0..nrows)
        .map(|row| (0..ncols).map(|col| sample(row, col)).collect())
        .collect()
}

fn dash_style(index: usize) -> DashType {
    match index % 6 {
        0 => DashType::Solid,
        1 => DashType::Dash,
        2 => DashType::Dot,
        3 => DashType::DashDot,
        4 => DashType::LongDash,
        _ => DashType::LongDashDot,
    }
}

fn symmetric_complex_plane_half_ranges(series: &[LineSeries]) -> (f64, f64) {
    let x_max_abs = series
        .iter()
        .flat_map(|trace| trace.x.iter())
        .map(|value| value.abs())
        .fold(0.0_f64, f64::max);
    let y_max_abs = series
        .iter()
        .flat_map(|trace| trace.y.iter())
        .map(|value| value.abs())
        .fold(0.0_f64, f64::max);

    (
        if x_max_abs > 0.0 {
            x_max_abs * 1.1
        } else {
            1.0
        },
        if y_max_abs > 0.0 {
            y_max_abs * 1.1
        } else {
            1.0
        },
    )
}

/// Creates an evenly spaced linear grid.
#[must_use]
pub fn linspace(start: f64, stop: f64, n: usize) -> Vec<f64> {
    if n <= 1 {
        return vec![start];
    }
    let step = (stop - start) / ((n - 1) as f64);
    (0..n).map(|index| start + (index as f64) * step).collect()
}

/// Creates an evenly spaced logarithmic grid in base-10 exponent space.
#[must_use]
pub fn logspace(log10_start: f64, log10_stop: f64, n: usize) -> Vec<f64> {
    if n <= 1 {
        return vec![10.0_f64.powf(log10_start)];
    }
    let step = (log10_stop - log10_start) / ((n - 1) as f64);
    (0..n)
        .map(|index| 10.0_f64.powf(log10_start + (index as f64) * step))
        .collect()
}
