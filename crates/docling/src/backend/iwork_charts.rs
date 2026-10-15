//! Reader for the charts of an iWork 2013+ document — docling's
//! `iwork/charts.py` (docling#4376, #466).
//!
//! Pages, Numbers and Keynote draw their charts with one shared engine,
//! `TSCH`: a `TSCH.ChartDrawableArchive` places the chart on the page and
//! carries the `TSCH.ChartArchive` describing it. No app keeps a picture of a
//! chart — the archive holds the model it is drawn from: its kind, the data it
//! plots, and a reference to the non-style archive holding its title. That is
//! what is read here; the field numbers are the ones docling verified against
//! real charts Apple wrote (via the apps' protobuf descriptors).

use super::iwork::{first_bytes, first_varint, Archive, Fields, Value};
use super::pages::{Chart, ChartSeries};
use super::pages_iwa::{reference_field, Objects};

/// Message type of `TSCH.ChartDrawableArchive`, a chart placed on a page.
pub(crate) const TSCH_CHART_DRAWABLE: u32 = 5021;
/// Extension field of the drawable holding its `TSCH.ChartArchive`.
const DRAWABLE_CHART_FIELD: u32 = 10000;

// `TSCH.ChartArchive`.
const CHART_TYPE_FIELD: u32 = 1;
const CHART_SERIES_DIRECTION_FIELD: u32 = 5;
const CHART_GRID_FIELD: u32 = 7;
const CHART_NON_STYLE_FIELD: u32 = 10;

// `TSCH.ChartGridArchive` and the `TSCH.GridRow` it holds.
const GRID_ROW_NAME_FIELD: u32 = 1;
const GRID_COLUMN_NAME_FIELD: u32 = 2;
const GRID_ROW_FIELD: u32 = 3;
const GRID_ROW_VALUE_FIELD: u32 = 1;
const GRID_VALUE_NUMBER_FIELD: u32 = 1;

/// `TSCH.SeriesDirection` of a chart plotting each row of its grid as a
/// series; a chart that does not say plots each column.
const SERIES_BY_ROW: u64 = 1;

/// `TSCH.ChartNonStyleArchive`, which holds a chart's title, in the property
/// map of its extension field: `tschchartinfodefaultshowtitle` and
/// `tschchartinfodefaulttitle`. A chart stores a title whether or not it
/// shows one, so it is only read when shown.
const TSCH_CHART_NON_STYLE: u32 = 5023;
const NON_STYLE_PROPERTIES_FIELD: u32 = 10000;
const NON_STYLE_SHOW_TITLE_FIELD: u32 = 21;
const NON_STYLE_TITLE_FIELD: u32 = 23;

/// The largest grid read, as rows × widest row: the size of the table the
/// data becomes. A grid past it is dropped whole rather than read in part.
const MAX_CHART_CELLS: usize = 100_000;

/// `TSCH.ChartType` → docling's picture classification label, the families
/// the PowerPoint and Excel backends classify into (`_CHART_LABELS`):
/// column and bar charts are `bar_chart`, pies and donuts `pie_chart`, the
/// 3D kinds the flat kind they extrude, anything else `other_chart`.
fn classification(chart_type: u64) -> &'static str {
    match chart_type {
        // column / bar (2D, stacked, 3D, interactive)
        1 | 2 | 6 | 7 | 12 | 13 | 17 | 18 | 20 | 21 => "bar_chart",
        // line (2D, 3D)
        3 | 14 => "line_chart",
        // pie / donut
        5 | 16 | 25 | 26 => "pie_chart",
        // scatter (2D, interactive)
        9 | 23 => "scatter_chart",
        // area, mixed / two-axis, bubble, radar, undefined
        _ => "other_chart",
    }
}

/// `iwa_chart`: one chart and the data it was last drawn from, or `None` when
/// the drawable carries no chart archive.
pub(crate) fn iwa_chart(drawable: &Archive, objects: &Objects) -> Option<Chart> {
    let raw = first_bytes(&drawable.payload, DRAWABLE_CHART_FIELD)?;
    let label = classification(first_varint(raw, CHART_TYPE_FIELD).unwrap_or(0));
    let by_row = first_varint(raw, CHART_SERIES_DIRECTION_FIELD) == Some(SERIES_BY_ROW);
    let (categories, series) = chart_grid(first_bytes(raw, CHART_GRID_FIELD), by_row);
    Some(Chart {
        label: label.to_string(),
        title: chart_title(raw, objects),
        categories,
        series,
    })
}

/// `iwa_chart_grid`: the categories and the series plotted across them. The
/// grid's size comes from its values, not its names: a name with nothing
/// under it is dropped, a value with no name kept under an empty one.
fn chart_grid(raw: Option<&[u8]>, by_row: bool) -> (Vec<String>, Vec<ChartSeries>) {
    let Some(grid) = raw else {
        return (Vec::new(), Vec::new());
    };
    let names = |field: u32| -> Vec<String> {
        Fields::new(grid)
            .filter_map(|(f, v)| match v {
                Value::Bytes(b) if f == field => Some(text(b)),
                _ => None,
            })
            .collect()
    };
    let row_names = names(GRID_ROW_NAME_FIELD);
    let column_names = names(GRID_COLUMN_NAME_FIELD);
    let mut rows: Vec<Vec<Option<f64>>> = Vec::new();
    let mut width = 0;
    for (f, v) in Fields::new(grid) {
        let Value::Bytes(row) = v else {
            continue;
        };
        if f != GRID_ROW_FIELD {
            continue;
        }
        let values: Vec<Option<f64>> = Fields::new(row)
            .filter_map(|(f, v)| match v {
                Value::Bytes(b) if f == GRID_ROW_VALUE_FIELD => Some(grid_value(b)),
                _ => None,
            })
            .collect();
        width = width.max(values.len());
        if (rows.len() + 1) * width > MAX_CHART_CELLS {
            return (Vec::new(), Vec::new());
        }
        rows.push(values);
    }
    if by_row {
        let categories = named(&column_names, width);
        let series = named(&row_names, rows.len())
            .into_iter()
            .zip(rows)
            .map(|(name, mut row)| {
                row.resize(width, None);
                ChartSeries { name, values: row }
            })
            .collect();
        return (categories, series);
    }
    let categories = named(&row_names, rows.len());
    let series = named(&column_names, width)
        .into_iter()
        .enumerate()
        .map(|(column, name)| ChartSeries {
            name,
            values: rows
                .iter()
                .map(|row| row.get(column).copied().flatten())
                .collect(),
        })
        .collect();
    (categories, series)
}

/// `iwa_chart_title`: the title a chart shows, from the non-style archive.
fn chart_title(chart: &[u8], objects: &Objects) -> Option<String> {
    let target = reference_field(chart, CHART_NON_STYLE_FIELD)?;
    let non_style = objects.get(&target).copied()?;
    if non_style.ty != TSCH_CHART_NON_STYLE {
        return None;
    }
    let properties = first_bytes(&non_style.payload, NON_STYLE_PROPERTIES_FIELD)?;
    if first_varint(properties, NON_STYLE_SHOW_TITLE_FIELD) != Some(1) {
        return None;
    }
    Some(text(first_bytes(properties, NON_STYLE_TITLE_FIELD)?)).filter(|t| !t.is_empty())
}

/// The number one `TSCH.GridValue` holds, or `None` when it has none (a date
/// or a duration leaves its point empty).
fn grid_value(raw: &[u8]) -> Option<f64> {
    Fields::new(raw)
        .find_map(|(f, v)| match v {
            Value::Fixed64(bits) if f == GRID_VALUE_NUMBER_FIELD => Some(f64::from_bits(bits)),
            _ => None,
        })
        .filter(|v| v.is_finite())
}

/// `_named`: `names` fitted to `count` entries, the missing ones `""`.
fn named(names: &[String], count: usize) -> Vec<String> {
    let mut out: Vec<String> = names.iter().take(count).cloned().collect();
    out.resize(count, String::new());
    out
}

fn text(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let low = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(low);
                return out;
            }
            out.push(low | 0x80);
        }
    }

    fn field_varint(n: u32, v: u64) -> Vec<u8> {
        let mut out = varint(u64::from(n) << 3);
        out.extend(varint(v));
        out
    }

    fn field_bytes(n: u32, b: &[u8]) -> Vec<u8> {
        let mut out = varint(u64::from(n) << 3 | 2);
        out.extend(varint(b.len() as u64));
        out.extend_from_slice(b);
        out
    }

    fn field_double(n: u32, v: f64) -> Vec<u8> {
        let mut out = varint(u64::from(n) << 3 | 1);
        out.extend_from_slice(&v.to_bits().to_le_bytes());
        out
    }

    fn grid(row_names: &[&str], column_names: &[&str], rows: &[&[Option<f64>]]) -> Vec<u8> {
        let mut g = Vec::new();
        for n in row_names {
            g.extend(field_bytes(GRID_ROW_NAME_FIELD, n.as_bytes()));
        }
        for n in column_names {
            g.extend(field_bytes(GRID_COLUMN_NAME_FIELD, n.as_bytes()));
        }
        for row in rows {
            let mut r = Vec::new();
            for v in row.iter() {
                let value = match v {
                    Some(x) => field_double(GRID_VALUE_NUMBER_FIELD, *x),
                    None => Vec::new(),
                };
                r.extend(field_bytes(GRID_ROW_VALUE_FIELD, &value));
            }
            g.extend(field_bytes(GRID_ROW_FIELD, &r));
        }
        g
    }

    /// The pie of docling's test: five rows named after the wedges, one
    /// column "Amount", plotted by row; its title shown.
    #[test]
    fn a_pie_by_row_with_a_shown_title() {
        let g = grid(
            &["Home", "Food"],
            &["Amount"],
            &[&[Some(-872.4)], &[Some(-226.0)]],
        );
        let mut chart = field_varint(CHART_TYPE_FIELD, 5);
        chart.extend(field_varint(CHART_SERIES_DIRECTION_FIELD, SERIES_BY_ROW));
        chart.extend(field_bytes(CHART_GRID_FIELD, &g));
        chart.extend(field_bytes(CHART_NON_STYLE_FIELD, &field_varint(1, 77)));
        let drawable = Archive {
            id: 1,
            ty: TSCH_CHART_DRAWABLE,
            payload: field_bytes(DRAWABLE_CHART_FIELD, &chart),
        };
        let mut props = field_varint(NON_STYLE_SHOW_TITLE_FIELD, 1);
        props.extend(field_bytes(
            NON_STYLE_TITLE_FIELD,
            b"Expenditure by Category",
        ));
        let non_style = Archive {
            id: 77,
            ty: TSCH_CHART_NON_STYLE,
            payload: field_bytes(NON_STYLE_PROPERTIES_FIELD, &props),
        };
        let objects: Objects = HashMap::from([(1, &drawable), (77, &non_style)]);
        let chart = iwa_chart(&drawable, &objects).expect("a chart");
        assert_eq!(chart.label, "pie_chart");
        assert_eq!(chart.title.as_deref(), Some("Expenditure by Category"));
        assert_eq!(chart.categories, ["Amount"]);
        let series: Vec<(&str, &[Option<f64>])> = chart
            .series
            .iter()
            .map(|s| (s.name.as_str(), s.values.as_slice()))
            .collect();
        assert_eq!(
            series,
            [("Home", &[Some(-872.4)][..]), ("Food", &[Some(-226.0)][..])]
        );
        let table = chart.table().expect("data");
        assert_eq!(
            table.rows,
            [["", "Home", "Food"], ["Amount", "-872.4", "-226"]]
        );
    }

    /// By column (the default), a value with no name is kept under an empty
    /// one and a short row is padded; a hidden title is no title.
    #[test]
    fn by_column_pads_and_names_the_unnamed() {
        let g = grid(&["a"], &[], &[&[Some(1.0), Some(2.5)], &[None]]);
        let mut chart = field_varint(CHART_TYPE_FIELD, 3);
        chart.extend(field_bytes(CHART_GRID_FIELD, &g));
        let drawable = Archive {
            id: 1,
            ty: TSCH_CHART_DRAWABLE,
            payload: field_bytes(DRAWABLE_CHART_FIELD, &chart),
        };
        let objects: Objects = HashMap::from([(1, &drawable)]);
        let chart = iwa_chart(&drawable, &objects).expect("a chart");
        assert_eq!(chart.label, "line_chart");
        assert_eq!(chart.title, None);
        assert_eq!(chart.categories, ["a", ""]);
        assert_eq!(chart.series.len(), 2);
        assert_eq!(chart.series[0].values, [Some(1.0), None]);
        assert_eq!(chart.series[1].values, [Some(2.5), None]);
        let table = chart.table().expect("data");
        assert_eq!(table.rows, [["", "", ""], ["a", "1", "2.5"], ["", "", ""]]);
    }
}
