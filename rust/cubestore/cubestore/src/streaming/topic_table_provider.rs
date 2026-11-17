use crate::metastore::Column;
use crate::queryplanner::udfs::{registerable_aggregate_udfs_iter, registerable_scalar_udfs_iter};
use crate::CubeError;
use async_trait::async_trait;
use datafusion::arrow::array::{
    Array, StringArray, StringBuilder, TimestampMicrosecondArray, TimestampMicrosecondBuilder,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use datafusion::catalog::Session;
use datafusion::common::TableReference;
use datafusion::config::ConfigOptions;
use datafusion::datasource::{provider_as_source, TableProvider, TableType};
use datafusion::error::DataFusionError;
use datafusion::execution::SessionStateDefaults;
use datafusion::logical_expr::{
    AggregateUDF, Expr, ScalarUDF, ScalarUDFImpl, Signature, TableSource, TypeSignature,
    Volatility, WindowUDF,
};
use datafusion::physical_plan::empty::EmptyExec;
use datafusion::physical_plan::ColumnarValue;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::scalar::ScalarValue;
use datafusion::sql::planner::ContextProvider;
use jiff::fmt::strtime::{self, BrokenDownTime};
use jiff::tz::{AmbiguousOffset, Offset, TimeZone};
use jiff::Timestamp;
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TopicTableProvider {
    topic: String,
    schema: SchemaRef,
    config_options: ConfigOptions,
    udfs: HashMap<String, Arc<ScalarUDF>>,
    udafs: HashMap<String, Arc<AggregateUDF>>,
    udwfs: HashMap<String, Arc<WindowUDF>>,
}

impl TopicTableProvider {
    pub fn new(topic: String, columns: &Vec<Column>) -> Self {
        let schema = Arc::new(Schema::new(
            columns
                .iter()
                .map(|c| c.clone().into())
                .collect::<Vec<Field>>(),
        ));
        let mut udfs = SessionStateDefaults::default_scalar_functions();
        udfs.extend(registerable_scalar_udfs_iter().map(Arc::new));
        udfs.push(Arc::new(
            ScalarUDF::new_from_impl(ParseTimestampFunc::new()),
        ));
        udfs.push(Arc::new(ScalarUDF::new_from_impl(ConvertTzFunc::new())));
        udfs.push(Arc::new(ScalarUDF::new_from_impl(
            FormatTimestampFunc::new(),
        )));

        let udfs = udfs
            .into_iter()
            .map(|udf| (udf.name().to_owned(), udf))
            .collect();

        let mut udafs = SessionStateDefaults::default_aggregate_functions();
        udafs.extend(registerable_aggregate_udfs_iter().map(Arc::new));

        let udafs = udafs
            .into_iter()
            .map(|udaf| (udaf.name().to_owned(), udaf))
            .collect();

        let udwfs = SessionStateDefaults::default_window_functions();
        let udwfs = udwfs
            .into_iter()
            .map(|udwf| (udwf.name().to_owned(), udwf))
            .collect();
        Self {
            topic,
            schema,
            config_options: ConfigOptions::default(),
            udfs,
            udafs,
            udwfs,
        }
    }
}

impl ContextProvider for TopicTableProvider {
    fn get_table_source(
        &self,
        name: TableReference,
    ) -> Result<Arc<dyn TableSource>, DataFusionError> {
        match name {
            TableReference::Bare { table } if table.as_ref() == self.topic => {
                Ok(provider_as_source(Arc::new(self.clone())))
            }
            _ => Err(DataFusionError::Plan(format!(
                "Topic table {} is not found",
                name
            ))),
        }
    }

    fn get_function_meta(&self, name: &str) -> Option<Arc<ScalarUDF>> {
        self.udfs.get(&name.to_ascii_lowercase()).cloned()
    }

    fn get_aggregate_meta(&self, name: &str) -> Option<Arc<AggregateUDF>> {
        self.udafs.get(&name.to_ascii_lowercase()).cloned()
    }

    fn get_window_meta(&self, name: &str) -> Option<Arc<WindowUDF>> {
        self.udwfs.get(&name.to_ascii_lowercase()).cloned()
    }

    fn get_variable_type(&self, _variable_names: &[String]) -> Option<DataType> {
        None
    }

    fn options(&self) -> &ConfigOptions {
        &self.config_options
    }

    fn udf_names(&self) -> Vec<String> {
        self.udfs.keys().cloned().collect()
    }

    fn udaf_names(&self) -> Vec<String> {
        self.udafs.keys().cloned().collect()
    }

    fn udwf_names(&self) -> Vec<String> {
        self.udwfs.keys().cloned().collect()
    }
}

#[async_trait]
impl TableProvider for TopicTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        _projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>, DataFusionError> {
        Ok(Arc::new(EmptyExec::new(self.schema())))
    }
}

fn sql_format_to_strformat(sql_format: &str) -> String {
    sql_format
        .replace("yyyy", "%Y")
        .replace("MM", "%m")
        .replace("dd", "%d")
        .replace("HH", "%H")
        .replace("mm", "%M")
        .replace("ss", "%S")
        .replace(".SSS", "%.3f")
        .replace("X", "Z") //FIXME there are no option for timezone offset with Z as zero in rust
        //so we alway expect Z at the end of the string
        .replace("'T'", "T")
}

/// Parses `input` with a strftime `format` as a wall clock time in `tz` and returns microseconds
/// since the Unix epoch.
///
/// The format has to yield a date, an hour and a minute. A wall clock time that `tz` skips (DST
/// gap) is an error, and so is one that occurs twice (DST fold) unless the input carries an
/// offset telling the two apart. A parsed offset has to agree with `tz`.
fn parse_timestamp_micros(input: &str, format: &str, tz: &TimeZone) -> Result<i64, String> {
    let parsed = BrokenDownTime::parse(format, input).map_err(|e| e.to_string())?;
    if parsed.hour().is_none() || parsed.minute().is_none() {
        return Err("input is not enough for unique date and time".to_string());
    }
    let wall_clock = parsed.to_datetime().map_err(|e| e.to_string())?;
    let parsed_offset = parsed.offset();
    let matches_parsed = |offset: Offset| parsed_offset.map_or(true, |parsed| parsed == offset);

    let ambiguous = tz.to_ambiguous_timestamp(wall_clock);
    let timestamp = match ambiguous.offset() {
        AmbiguousOffset::Unambiguous { offset } if matches_parsed(offset) => {
            ambiguous.unambiguous()
        }
        AmbiguousOffset::Fold { before, after } => {
            match (matches_parsed(before), matches_parsed(after)) {
                (true, false) => ambiguous.earlier(),
                (false, true) => ambiguous.later(),
                (true, true) => {
                    return Err("input is not enough for unique date and time".to_string())
                }
                (false, false) => {
                    return Err("no possible date and time matching input".to_string())
                }
            }
        }
        AmbiguousOffset::Unambiguous { .. } | AmbiguousOffset::Gap { .. } => {
            return Err("no possible date and time matching input".to_string())
        }
    }
    .map_err(|e| e.to_string())?;

    // Floor rather than truncate, so instants before 1970 land on the earlier microsecond.
    i64::try_from(timestamp.as_nanosecond().div_euclid(1_000)).map_err(|e| e.to_string())
}

/// The UTC wall clock of a `TimestampMicrosecond` value.
fn micros_to_wall_clock(micros: i64) -> Result<jiff::civil::DateTime, jiff::Error> {
    Ok(Offset::UTC.to_datetime(Timestamp::from_microsecond(micros)?))
}

/// Reads `micros` as a wall clock time in `from_tz` and returns the wall clock time `to_tz` shows
/// at the same instant. Of a wall clock time that occurs twice in `from_tz`, the earlier one wins.
fn convert_tz_micros(
    micros: i64,
    from_tz: &TimeZone,
    to_tz: &TimeZone,
) -> Result<i64, DataFusionError> {
    let error =
        || DataFusionError::Execution(format!("Can't convert timezone for timestamp {}", micros));
    let wall_clock = micros_to_wall_clock(micros).map_err(|_| error())?;
    let ambiguous = from_tz.to_ambiguous_timestamp(wall_clock);
    if matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. }) {
        return Err(error());
    }
    let instant = ambiguous.earlier().map_err(|_| error())?;
    Offset::UTC
        .to_timestamp(to_tz.to_datetime(instant))
        .map(|converted| converted.as_microsecond())
        .map_err(|_| error())
}

fn format_timestamp_micros(micros: i64, format: &str) -> Result<String, DataFusionError> {
    micros_to_wall_clock(micros)
        .and_then(|wall_clock| strtime::format(format, wall_clock))
        .map_err(|e| {
            DataFusionError::Execution(format!(
                "Error while formatting timestamp {}: {}",
                micros, e
            ))
        })
}

fn parse_timestamp_array(
    input: &StringArray,
    tz: &TimeZone,
    format: &str,
) -> Result<TimestampMicrosecondArray, DataFusionError> {
    let mut result = TimestampMicrosecondBuilder::new();
    for i in 0..input.len() {
        if input.is_null(i) {
            result.append_null();
        } else {
            let micros = match parse_timestamp_micros(input.value(i), format, tz) {
                Ok(micros) => micros,
                Err(e) => {
                    return Err(DataFusionError::Execution(format!(
                        "Error while parsing timestamp `{}`: {}",
                        input.value(i),
                        e
                    )));
                }
            };
            result.append_value(micros);
        }
    }
    Ok(result.finish())
}

fn convert_tz_array(
    input: &TimestampMicrosecondArray,
    from_tz: &TimeZone,
    to_tz: &TimeZone,
) -> Result<TimestampMicrosecondArray, DataFusionError> {
    let mut result = TimestampMicrosecondBuilder::new();
    if from_tz == to_tz {
        for i in 0..input.len() {
            if input.is_null(i) {
                result.append_null();
            } else {
                result.append_value(input.value(i));
            }
        }
    } else {
        for i in 0..input.len() {
            if input.is_null(i) {
                result.append_null();
            } else {
                result.append_value(convert_tz_micros(input.value(i), from_tz, to_tz)?);
            }
        }
    }
    Ok(result.finish())
}
fn format_timestamp_array(
    input: &TimestampMicrosecondArray,
    format: &str,
) -> Result<StringArray, DataFusionError> {
    let mut result = StringBuilder::new();
    for i in 0..input.len() {
        if input.is_null(i) {
            result.append_null();
        } else {
            result.append_value(format_timestamp_micros(input.value(i), format)?);
        }
    }
    Ok(result.finish())
}

struct ParseTimestampFunc {
    signature: Signature,
}

impl Debug for ParseTimestampFunc {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ParseTimestampFunc")
    }
}

impl ParseTimestampFunc {
    fn new() -> ParseTimestampFunc {
        ParseTimestampFunc {
            signature: Signature::one_of(
                vec![
                    TypeSignature::Exact(vec![DataType::Utf8, DataType::Utf8, DataType::Utf8]),
                    TypeSignature::Exact(vec![DataType::Utf8, DataType::Utf8]),
                ],
                Volatility::Stable,
            ),
        }
    }
}

impl ScalarUDFImpl for ParseTimestampFunc {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "parse_timestamp"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Timestamp(TimeUnit::Microsecond, None))
    }

    fn invoke(&self, inputs: &[ColumnarValue]) -> datafusion::common::Result<ColumnarValue> {
        if inputs.len() < 2 || inputs.len() > 3 {
            return Err(DataFusionError::Execution(
                "Expected 2 or 3 arguments in PARSE_TIMESTAMP".to_string(),
            ));
        }

        let format = match &inputs[1] {
            ColumnarValue::Scalar(ScalarValue::Utf8(Some(v))) => sql_format_to_strformat(v),
            _ => {
                return Err(DataFusionError::Execution(
                    "Only scalar arguments are supported as format in PARSE_TIMESTAMP".to_string(),
                ));
            }
        };
        let tz: TimeZone = if inputs.len() == 3 {
            match &inputs[2] {
                ColumnarValue::Scalar(ScalarValue::Utf8(Some(s))) => {
                    TimeZone::get(s).map_err(|_| {
                        CubeError::user(format!("Incorrect timezone {} in PARSE_TIMESTAMP", s))
                    })?
                }
                _ => {
                    return Err(DataFusionError::Execution(
                        "Only scalar arguments are supported as timezone in PARSE_TIMESTAMP"
                            .to_string(),
                    ));
                }
            }
        } else {
            TimeZone::UTC
        };

        match &inputs[0] {
            ColumnarValue::Scalar(ScalarValue::Utf8(Some(s))) => {
                let micros = match parse_timestamp_micros(s, &format, &tz) {
                    Ok(micros) => micros,
                    Err(e) => {
                        return Err(DataFusionError::Execution(format!(
                            "Error while parsing timestamp: {}",
                            e
                        )));
                    }
                };
                Ok(ColumnarValue::Scalar(ScalarValue::TimestampMicrosecond(
                    Some(micros),
                    None,
                )))
            }
            ColumnarValue::Array(t) if t.as_any().is::<StringArray>() => {
                let t = t.as_any().downcast_ref::<StringArray>().unwrap();
                Ok(ColumnarValue::Array(Arc::new(parse_timestamp_array(
                    &t, &tz, &format,
                )?)))
            }
            _ => {
                return Err(DataFusionError::Execution(
                    "First argument in PARSE_TIMESTAMP must be string or array of strings"
                        .to_string(),
                ));
            }
        }
    }
}

struct ConvertTzFunc {
    signature: Signature,
}

impl ConvertTzFunc {
    fn new() -> ConvertTzFunc {
        ConvertTzFunc {
            signature: Signature::exact(
                vec![
                    DataType::Timestamp(TimeUnit::Microsecond, None),
                    DataType::Utf8,
                    DataType::Utf8,
                ],
                Volatility::Stable,
            ),
        }
    }
}

impl Debug for ConvertTzFunc {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConvertTzFunc")
    }
}

impl ScalarUDFImpl for ConvertTzFunc {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "convert_tz_ksql"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Timestamp(TimeUnit::Microsecond, None))
    }

    fn invoke(&self, inputs: &[ColumnarValue]) -> datafusion::common::Result<ColumnarValue> {
        if inputs.len() != 3 {
            return Err(DataFusionError::Execution(
                "Expected 3 arguments in CONVERT_TZ_KSQL".to_string(),
            ));
        }

        let from_tz: TimeZone = match &inputs[1] {
            ColumnarValue::Scalar(ScalarValue::Utf8(Some(s))) => {
                TimeZone::get(s).map_err(|_| {
                    CubeError::user(format!("Incorrect timezone {} in CONVERT_TZ_KSQL", s))
                })?
            }
            _ => {
                return Err(DataFusionError::Execution(
                    "Only scalar arguments are supported as from_timezone in CONVERT_TZ_KSQL"
                        .to_string(),
                ));
            }
        };

        let to_tz: TimeZone = match &inputs[2] {
            ColumnarValue::Scalar(ScalarValue::Utf8(Some(s))) => {
                TimeZone::get(s).map_err(|_| {
                    CubeError::user(format!("Incorrect timezone {} in CONVERT_TZ_KSQL", s))
                })?
            }
            _ => {
                return Err(DataFusionError::Execution(
                    "Only scalar arguments are supported as to_timezone in CONVERT_TZ_KSQL"
                        .to_string(),
                ));
            }
        };
        match &inputs[0] {
            ColumnarValue::Scalar(ScalarValue::TimestampMicrosecond(Some(t), None)) => {
                if from_tz == to_tz {
                    Ok(ColumnarValue::Scalar(ScalarValue::TimestampMicrosecond(
                        Some(*t),
                        None,
                    )))
                } else {
                    Ok(ColumnarValue::Scalar(ScalarValue::TimestampMicrosecond(
                        Some(convert_tz_micros(*t, &from_tz, &to_tz)?),
                        None,
                    )))
                }
            }
            ColumnarValue::Array(t) if t.as_any().is::<TimestampMicrosecondArray>() => {
                let t = t
                    .as_any()
                    .downcast_ref::<TimestampMicrosecondArray>()
                    .unwrap();
                Ok(ColumnarValue::Array(Arc::new(convert_tz_array(
                    t, &from_tz, &to_tz,
                )?)))
            }
            _ => {
                return Err(DataFusionError::Execution(
                    "First argument in CONVERT_TZ_KSQL must be timestamp or array of timestamps"
                        .to_string(),
                ));
            }
        }
    }
}

struct FormatTimestampFunc {
    signature: Signature,
}

impl FormatTimestampFunc {
    fn new() -> FormatTimestampFunc {
        FormatTimestampFunc {
            signature: Signature::exact(
                vec![
                    DataType::Timestamp(TimeUnit::Microsecond, None),
                    DataType::Utf8,
                ],
                Volatility::Stable,
            ),
        }
    }
}

impl Debug for FormatTimestampFunc {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "FormatTimestampFunc")
    }
}

impl ScalarUDFImpl for FormatTimestampFunc {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        "format_timestamp"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Utf8)
    }

    fn invoke(&self, inputs: &[ColumnarValue]) -> datafusion::common::Result<ColumnarValue> {
        if inputs.len() != 2 {
            return Err(DataFusionError::Execution(
                "Expected 2 arguments in FORMAT_TIMESTAMP".to_string(),
            ));
        }

        let format = match &inputs[1] {
            ColumnarValue::Scalar(ScalarValue::Utf8(Some(v))) => sql_format_to_strformat(v),
            _ => {
                return Err(DataFusionError::Execution(
                    "Only scalar arguments are supported as format in FORMAT_TIMESTAMP".to_string(),
                ));
            }
        };

        match &inputs[0] {
            ColumnarValue::Scalar(ScalarValue::TimestampMicrosecond(Some(t), None)) => {
                Ok(ColumnarValue::Scalar(ScalarValue::Utf8(Some(
                    format_timestamp_micros(*t, &format)?,
                ))))
            }
            ColumnarValue::Array(t) if t.as_any().is::<TimestampMicrosecondArray>() => {
                let t = t
                    .as_any()
                    .downcast_ref::<TimestampMicrosecondArray>()
                    .unwrap();
                Ok(ColumnarValue::Array(Arc::new(format_timestamp_array(
                    &t, &format,
                )?)))
            }
            _ => {
                return Err(DataFusionError::Execution(
                    "First argument in FORMAT_TIMESTAMP must be timestamp or array of timestamps"
                        .to_string(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn micros(rfc3339: &str) -> i64 {
        rfc3339.parse::<Timestamp>().unwrap().as_microsecond()
    }

    fn tz(name: &str) -> TimeZone {
        TimeZone::get(name).unwrap()
    }

    #[test]
    fn parse_timestamp_in_time_zone() {
        let parse = |input: &str, format: &str, zone: &str| {
            parse_timestamp_micros(input, &sql_format_to_strformat(format), &tz(zone))
        };

        assert_eq!(
            parse(
                "2023-06-05T03:00:00.300Z",
                "yyyy-MM-dd'T'HH:mm:ss.SSSX",
                "UTC"
            ),
            Ok(micros("2023-06-05T03:00:00.3Z"))
        );
        // The fraction is optional.
        assert_eq!(
            parse("2023-06-05T03:00:00Z", "yyyy-MM-dd'T'HH:mm:ss.SSSX", "UTC"),
            Ok(micros("2023-06-05T03:00:00Z"))
        );
        assert_eq!(
            parse(
                "2023-06-05 06:12:23",
                "yyyy-MM-dd HH:mm:ss",
                "Europe/Istanbul"
            ),
            Ok(micros("2023-06-05T03:12:23Z"))
        );
        assert_eq!(
            parse("1969-12-31 23:59:59.999", "yyyy-MM-dd HH:mm:ss.SSS", "UTC"),
            Ok(-1_000)
        );

        // A date alone is not enough.
        assert!(parse("2023-06-05", "yyyy-MM-dd", "UTC").is_err());
        assert!(parse("2023-02-30 00:00:00", "yyyy-MM-dd HH:mm:ss", "UTC").is_err());
        assert!(parse("2023-06-05 03:00:00 ", "yyyy-MM-dd HH:mm:ss", "UTC").is_err());
    }

    #[test]
    fn parse_timestamp_around_dst_transitions() {
        let new_york = tz("America/New_York");
        let parse = |input: &str, format: &str| parse_timestamp_micros(input, format, &new_york);

        // 02:30 does not exist on the day clocks spring forward.
        assert!(parse("2023-03-12 02:30:00", "%Y-%m-%d %H:%M:%S").is_err());
        // 01:30 occurs twice on the day clocks fall back: ambiguous without an offset.
        assert!(parse("2023-11-05 01:30:00", "%Y-%m-%d %H:%M:%S").is_err());
        assert_eq!(
            parse("2023-11-05 01:30:00 -0400", "%Y-%m-%d %H:%M:%S %z"),
            Ok(micros("2023-11-05T05:30:00Z"))
        );
        assert_eq!(
            parse("2023-11-05 01:30:00 -0500", "%Y-%m-%d %H:%M:%S %z"),
            Ok(micros("2023-11-05T06:30:00Z"))
        );
        // An offset the time zone does not have at that time.
        assert!(parse("2023-11-05 01:30:00 -0600", "%Y-%m-%d %H:%M:%S %z").is_err());
        assert!(parse("2023-06-05 03:00:00 +0300", "%Y-%m-%d %H:%M:%S %z").is_err());
        assert_eq!(
            parse("2023-06-05 03:00:00 -0400", "%Y-%m-%d %H:%M:%S %z"),
            Ok(micros("2023-06-05T07:00:00Z"))
        );
    }

    #[test]
    fn convert_tz_around_dst_transitions() {
        let convert = |wall_clock: &str, from: &str, to: &str| {
            convert_tz_micros(micros(wall_clock), &tz(from), &tz(to)).ok()
        };

        assert_eq!(
            convert("2023-06-05T03:12:23.3Z", "UTC", "Europe/Istanbul"),
            Some(micros("2023-06-05T06:12:23.3Z"))
        );
        assert_eq!(
            convert(
                "2023-06-05T03:12:23Z",
                "Asia/Krasnoyarsk",
                "Europe/Istanbul"
            ),
            Some(micros("2023-06-04T23:12:23Z"))
        );
        // A wall clock time that occurs twice resolves to the earlier instant.
        assert_eq!(
            convert("2023-11-05T01:30:00Z", "America/New_York", "UTC"),
            Some(micros("2023-11-05T05:30:00Z"))
        );
        // One that does not exist can not be converted.
        assert_eq!(
            convert("2023-03-12T02:30:00Z", "America/New_York", "UTC"),
            None
        );
    }

    #[test]
    fn format_timestamp() {
        let format = |timestamp: &str, format: &str| {
            format_timestamp_micros(micros(timestamp), &sql_format_to_strformat(format)).ok()
        };

        assert_eq!(
            format("2023-06-05T03:00:00.3Z", "yyyy-MM-dd'T'HH:mm:ss.SSSX").as_deref(),
            Some("2023-06-05T03:00:00.300Z")
        );
        assert_eq!(
            format("2023-06-05T03:00:00.123456Z", "yyyy-MM-dd HH:mm:ss.SSS").as_deref(),
            Some("2023-06-05 03:00:00.123")
        );
        assert_eq!(
            format("1969-12-31T23:59:59.5Z", "yyyyMMddHH").as_deref(),
            Some("1969123123")
        );
        // An invalid format is an error, not a panic.
        assert_eq!(format("2023-06-05T03:00:00Z", "%!"), None);
    }
}
