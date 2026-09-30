use super::*;

/// unit tier -- pure in-memory: the only unit under test is the shared
/// advisory wording classifier `wording_warning`; no filesystem, database,
/// git or process (`global.constraint.test-tier-boundaries`).
mod unit {
    use super::*;

    /// `wording_warning` flags the three word classes a reviewer is likely to
    /// reject in a tier-1-3 node body — negation (`not`/`never`/`no`),
    /// future-tense (`will`/`shall`) and time-relative
    /// (`today`/`now`/`currently`/`no longer`/`was`) — returning the shared
    /// [`WORDING_ADVISORY`], and is silent on plain present-tense prose.
    #[test]
    fn wording_warning_flags_negation_future_and_time_words() {
        // Negation.
        for body in [
            "The system does not store passwords.",
            "Never delete a record.",
            "No customer sees another customer's data.",
        ] {
            assert_eq!(
                wording_warning(body),
                Some(WORDING_ADVISORY),
                "negation must be flagged: {body}"
            );
        }

        // Future-tense / obligation.
        for body in [
            "The service will retry the request.",
            "The operator shall approve every change.",
        ] {
            assert_eq!(
                wording_warning(body),
                Some(WORDING_ADVISORY),
                "future-tense wording must be flagged: {body}"
            );
        }

        // Time-relative.
        for body in [
            "Today the price is fixed.",
            "The job now runs hourly.",
            "Currently the queue drains nightly.",
            "The account is no longer active.",
            "The old flow was synchronous.",
        ] {
            assert_eq!(
                wording_warning(body),
                Some(WORDING_ADVISORY),
                "time-relative wording must be flagged: {body}"
            );
        }

        // Plain present-tense prose is silent.
        for body in [
            "A customer places an order.",
            "The service stores the record.",
            "Each order carries a total.",
            "The system reuses the cached value.",
            "",
        ] {
            assert_eq!(
                wording_warning(body),
                None,
                "plain present-tense prose must stay silent: {body}"
            );
        }
    }
}
