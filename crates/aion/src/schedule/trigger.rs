//! Trigger parsing and deterministic next-fire-time calculation.

use std::str::FromStr;

use aion_core::TriggerSpec;
use chrono::{DateTime, Utc};

/// Errors returned while validating or evaluating schedule triggers.
#[derive(thiserror::Error, Debug)]
pub enum ScheduleError {
    /// The supplied cron expression is empty or contains only whitespace.
    #[error("cron expression must not be empty")]
    EmptyCronExpression,

    /// The supplied cron expression could not be parsed by the cron parser.
    #[error("invalid cron expression `{expression}`: {source}")]
    CronParse {
        /// The expression that failed validation.
        expression: String,
        /// Parser error returned by the underlying cron crate.
        source: saffron::parse::CronParseError,
    },

    /// The cron expression has no future matching timestamp.
    #[error("cron expression `{expression}` has no next fire time")]
    NoNextFireTime {
        /// The expression whose next time could not be found.
        expression: String,
    },

    /// Interval triggers must advance time by a positive duration.
    #[error("interval trigger period must be greater than zero")]
    ZeroInterval,

    /// The supplied interval cannot be represented by chrono's duration type.
    #[error("interval trigger period cannot be represented as a chrono duration")]
    IntervalOutOfRange,

    /// Adding the interval to the reference timestamp overflowed.
    #[error("next fire time overflowed the reference timestamp")]
    NextFireTimeOutOfRange,
}

/// Parse and validate a standard five-field cron expression.
///
/// The parser accepts expressions in the `minute hour day-of-month month day-of-week` form and
/// returns a typed [`ScheduleError`] for empty or invalid input.
///
/// # Errors
///
/// Returns [`ScheduleError::EmptyCronExpression`] for blank input and
/// [`ScheduleError::CronParse`] when the expression is not accepted by the cron parser.
pub fn parse_cron_expression(expression: &str) -> Result<saffron::Cron, ScheduleError> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Err(ScheduleError::EmptyCronExpression);
    }

    saffron::Cron::from_str(expression).map_err(|source| ScheduleError::CronParse {
        expression: expression.to_owned(),
        source,
    })
}

/// Return the next time a trigger should fire, strictly after `after`.
///
/// Cron triggers are parsed and evaluated with the cron parser's strict-after API. Interval
/// triggers add their period to the supplied reference timestamp using checked arithmetic.
///
/// # Errors
///
/// Returns [`ScheduleError`] when the cron expression is invalid or has no future time, when an
/// interval is zero, or when duration conversion/timestamp addition overflows.
pub fn next_fire_time(
    trigger: &TriggerSpec,
    after: DateTime<Utc>,
) -> Result<DateTime<Utc>, ScheduleError> {
    match trigger {
        TriggerSpec::Cron { expression } => {
            let cron = parse_cron_expression(expression)?;
            cron.next_after(after)
                .ok_or_else(|| ScheduleError::NoNextFireTime {
                    expression: expression.trim().to_owned(),
                })
        }
        TriggerSpec::Interval { period } => {
            if period.is_zero() {
                return Err(ScheduleError::ZeroInterval);
            }

            let chrono_duration = chrono::Duration::from_std(*period)
                .map_err(|_| ScheduleError::IntervalOutOfRange)?;
            after
                .checked_add_signed(chrono_duration)
                .ok_or(ScheduleError::NextFireTimeOutOfRange)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::time::Duration;

    use aion_core::TriggerSpec;
    use chrono::{DateTime, Utc};

    use super::{ScheduleError, next_fire_time, parse_cron_expression};

    fn parse_utc(value: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
        DateTime::parse_from_rfc3339(value).map(|date_time| date_time.with_timezone(&Utc))
    }

    #[test]
    fn parses_standard_five_field_cron_expressions() {
        assert!(parse_cron_expression("0 0 * * *").is_ok());
        assert!(parse_cron_expression("*/5 * * * *").is_ok());
    }

    #[test]
    fn invalid_cron_expressions_return_typed_errors() {
        assert!(matches!(
            parse_cron_expression("invalid"),
            Err(ScheduleError::CronParse { .. })
        ));
        assert!(matches!(
            parse_cron_expression(""),
            Err(ScheduleError::EmptyCronExpression)
        ));
        assert!(matches!(
            parse_cron_expression("   "),
            Err(ScheduleError::EmptyCronExpression)
        ));
    }

    #[test]
    fn midnight_cron_returns_next_midnight_strictly_after_reference() -> Result<(), Box<dyn Error>>
    {
        let trigger = TriggerSpec::Cron {
            expression: "0 0 * * *".to_owned(),
        };
        let reference = parse_utc("2026-06-07T00:00:00Z")?;
        let expected = parse_utc("2026-06-08T00:00:00Z")?;

        let next = next_fire_time(&trigger, reference)?;

        assert_eq!(next, expected);
        assert!(next > reference);
        Ok(())
    }

    #[test]
    fn interval_returns_reference_plus_period() -> Result<(), Box<dyn Error>> {
        let period = Duration::from_secs(5 * 60);
        let trigger = TriggerSpec::Interval { period };
        let reference = parse_utc("2026-06-07T12:00:00Z")?;

        let next = next_fire_time(&trigger, reference)?;

        assert_eq!(next, reference + chrono::Duration::minutes(5));
        assert!(next > reference);
        Ok(())
    }

    #[test]
    fn zero_interval_returns_typed_error() -> Result<(), Box<dyn Error>> {
        let trigger = TriggerSpec::Interval {
            period: Duration::ZERO,
        };
        let reference = parse_utc("2026-06-07T12:00:00Z")?;

        assert!(matches!(
            next_fire_time(&trigger, reference),
            Err(ScheduleError::ZeroInterval)
        ));
        Ok(())
    }

    #[test]
    fn successive_calls_are_strictly_increasing() -> Result<(), Box<dyn Error>> {
        let cron_trigger = TriggerSpec::Cron {
            expression: "*/5 * * * *".to_owned(),
        };
        let first = next_fire_time(&cron_trigger, parse_utc("2026-06-07T00:00:00Z")?)?;
        let second = next_fire_time(&cron_trigger, first)?;
        assert!(second > first);

        let interval_trigger = TriggerSpec::Interval {
            period: Duration::from_secs(60),
        };
        let third = next_fire_time(&interval_trigger, second)?;
        let fourth = next_fire_time(&interval_trigger, third)?;
        assert!(fourth > third);
        Ok(())
    }
}
