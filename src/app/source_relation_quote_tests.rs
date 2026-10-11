use super::*;
use std::{collections::BTreeMap, time::Duration};
fn money(currency: &str, n: u64) -> Money {
    Money::new(Currency::new(currency).unwrap(), n)
}
fn bound() -> AttemptBound {
    let h = Blake3Hash::digest([]);
    let mut card = RateCard {
        id: "quote-test".into(),
        version: 1,
        fingerprint: h.clone(),
        currency: Currency::new("USD").unwrap(),
        validity: PriceValidity::DispatchLocked {
            valid_from_utc_ms: 0,
            valid_until_utc_ms: 100_000,
        },
        request_fee_nanounits: 6_000_000_000,
        rates: BTreeMap::new(),
    };
    card.fingerprint = budgets::rate_card_fingerprint(&card).unwrap();
    let mut b = AttemptBound {
        codec: None,
        profile_fingerprint: None,
        capability: Capability::Generate,
        profile_id: "quote-test".into(),
        endpoint_fingerprint: h.clone(),
        config_fingerprint: h.clone(),
        input_hash: h.clone(),
        wire_hash: h.clone(),
        requested_model: Some("test".into()),
        requested_model_revision: None,
        request_bytes: 100,
        response_bytes: 100,
        timeout_ms: 1,
        applicable_classes: BTreeSet::new(),
        billable_bounds: BTreeMap::new(),
        rate_card: Some(card),
        quoted_allowance: Some(money("USD", 6_000_000_000)),
        bounds_fingerprint: h,
    };
    b.bounds_fingerprint = budgets::bound_fingerprint(&b).unwrap();
    b
}
#[test]
fn aggregate_over_cap_is_reportable_but_individual_over_cap_refuses() {
    let limits = LifetimeLimits {
        max_cost: Some(money("USD", 10_000_000_000)),
        ..LifetimeLimits::default()
    };
    let a = finite_quote(&bound(), &limits, 100, 200).unwrap();
    let mut total = budgets::zero();
    add_finite(&mut total, &a).unwrap();
    add_finite(&mut total, &a).unwrap();
    assert!(budgets::fits(&budgets::zero(), &budgets::zero(), &total, &limits).is_err());
    assert_eq!(total.cost.unwrap().nanounits(), 12_000_000_000);
    let mut limits = limits;
    limits.max_cost = Some(money("USD", 5_000_000_000));
    assert!(finite_quote(&bound(), &limits, 100, 200).is_err());
}
#[test]
fn finite_quote_refuses_unknown_cost_even_without_monetary_cap() {
    let mut b = bound();
    b.rate_card = None;
    b.quoted_allowance = None;
    b.bounds_fingerprint = budgets::bound_fingerprint(&b).unwrap();
    assert!(finite_quote(&b, &LifetimeLimits::default(), 100, 200).is_err());
}
#[test]
fn finite_quote_refuses_unknown_billable_class_and_expired_rates() {
    let mut b = bound();
    b.applicable_classes.insert(BillableClass::Input);
    b.billable_bounds
        .insert(BillableClass::Input, TokenBound::Unknown);
    b.quoted_allowance = None;
    b.bounds_fingerprint = budgets::bound_fingerprint(&b).unwrap();
    assert!(finite_quote(&b, &LifetimeLimits::default(), 100, 200).is_err());
    assert!(finite_quote(&bound(), &LifetimeLimits::default(), 100_000, 200_000).is_err());
}
#[test]
fn checked_aggregate_refuses_overflow_and_currency_mixing() {
    let mut a = budgets::zero();
    a.requests = u64::MAX;
    let mut b = budgets::zero();
    b.requests = 1;
    b.cost = Some(money("USD", 1));
    assert!(add_finite(&mut a, &b).is_err());
    let mut a = budgets::zero();
    a.cost = Some(money("USD", 1));
    b.cost = Some(money("EUR", 1));
    assert!(add_finite(&mut a, &b).is_err());
}
#[test]
fn output_serialization_refuses_whole_oversized_value_and_deadline() {
    let deadline = Instant::now() + Duration::from_secs(1);
    assert!(bounded_size(&"abcdef", 4, deadline).is_err());
    assert!(bounded_size(&vec!["a"; 10], 20, deadline).is_err());
    assert!(bounded_size(&"ok", 100, Instant::now()).is_err());
}
#[test]
fn quote_clock_rejects_either_regression_and_precreation() {
    let prior = ClockReading {
        utc_ms: 100,
        monotonic_ms: 100,
    };
    for now in [
        ClockReading {
            utc_ms: 99,
            monotonic_ms: 101,
        },
        ClockReading {
            utc_ms: 101,
            monotonic_ms: 99,
        },
    ] {
        assert!(validate_clock(now, 0, Some(prior)).is_err());
    }
    assert!(validate_clock(prior, 101, None).is_err());
    assert!(validate_clock(prior, 100, Some(prior)).is_ok());
}
