//! Bounded declarative documents rendered by Xedoc report surfaces.

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Value;

/// A script-authored report document.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportDocument {
    /// Non-empty report title.
    pub title: String,
    /// Ordered report sections.
    pub sections: Vec<ReportSection>,
}

/// One declarative report section.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReportSection {
    /// Compact label/value summary metrics.
    MetricGrid {
        /// Ordered metrics.
        metrics: Vec<ReportMetric>,
    },
    /// A multi-series line chart with shared X coordinates.
    LineChart {
        /// Non-empty chart title.
        title: String,
        /// Non-empty horizontal-axis label.
        #[serde(rename = "xAxis")]
        x_axis: String,
        /// Non-empty vertical-axis label.
        #[serde(rename = "yAxis")]
        y_axis: String,
        /// Ordered chart series.
        series: Vec<ReportLineSeries>,
    },
    /// A text-only data table.
    Table {
        /// Non-empty table title.
        title: String,
        /// Ordered column labels.
        columns: Vec<String>,
        /// Ordered table rows.
        rows: Vec<Vec<String>>,
    },
    /// A user-visible informational notice.
    Notice {
        /// Notice presentation level.
        level: ReportNoticeLevel,
        /// Notice text.
        text: String,
    },
}

/// One text metric.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportMetric {
    /// Non-empty metric label.
    pub label: String,
    /// Display-ready metric value.
    pub value: String,
}

/// One chart series.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportLineSeries {
    /// Non-empty series label.
    pub label: String,
    /// Host-defined bounded color.
    pub color: ReportColor,
    /// Points aligned by X coordinate with every other series.
    pub points: Vec<ReportLinePoint>,
}

/// One chart point or explicit gap.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportLinePoint {
    /// Display-ready X coordinate.
    pub x: String,
    /// Finite Y coordinate, or `null` for an explicit gap.
    #[serde(deserialize_with = "deserialize_report_point_value")]
    pub value: Option<f64>,
}

fn deserialize_report_point_value<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(None);
    }
    let value = value.get("value").cloned().unwrap_or(value);
    value
        .as_f64()
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("report point value must be a finite number"))
}

/// Safe host-defined chart colors.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReportColor {
    Blue,
    Green,
    Orange,
    Purple,
    Red,
    Teal,
}

/// Safe host-defined notice levels.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReportNoticeLevel {
    Info,
    Warning,
    Error,
}
