//! Vectors computed by the Allure 3 v3.33.0 source itself (see `tests/vectors/generate.sh`).

use fastproof_model::flaky::{FlakyOptions, is_flaky, status_transition, test_flakiness};
use fastproof_model::identity::{
    environment_hash, parameters_hash, retry_hash, stringify_parameters, test_case_hash,
};
use fastproof_model::{Parameter, ParameterMode, Status};
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("vectors/allure3-vectors.json")).unwrap()
}

fn opt_str(v: &Value) -> Option<&str> {
    v.as_str()
}

fn status(s: &str) -> Status {
    Status::parse_lenient(s)
}

#[test]
fn identity_vectors() {
    let v = vectors();
    let cases = v["identity"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for c in cases {
        let params: Vec<Parameter> = c["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| Parameter {
                name: p["name"].as_str().unwrap_or("").to_string(),
                value: p["value"].as_str().map(str::to_string),
                excluded: p["excluded"].as_bool().unwrap_or(false),
                mode: match p["mode"].as_str() {
                    Some("masked") => ParameterMode::Masked,
                    Some("hidden") => ParameterMode::Hidden,
                    _ => ParameterMode::Default,
                },
            })
            .collect();
        let name = c["name"].as_str().unwrap();
        let tch = test_case_hash(opt_str(&c["testCaseId"]), opt_str(&c["fullName"]));
        assert_eq!(tch.as_deref(), opt_str(&c["testCaseHash"]), "{name}");
        assert_eq!(
            stringify_parameters(&params),
            c["parametersString"].as_str().unwrap(),
            "{name}"
        );
        let ph = parameters_hash(&params);
        assert_eq!(ph, c["parametersHash"].as_str().unwrap(), "{name}");
        let eh = environment_hash(opt_str(&c["env"]));
        assert_eq!(eh.as_deref(), opt_str(&c["environmentHash"]), "{name}");
        let rh = retry_hash(tch.as_deref().unwrap(), &ph, eh.as_deref());
        assert_eq!(rh, c["retryHash"].as_str().unwrap(), "{name}");
    }
}

#[test]
fn flaky_vectors() {
    let v = vectors();
    for c in v["flaky"].as_array().unwrap() {
        let seq: Vec<&str> = c["chronological"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        let (current, past) = seq.split_last().unwrap();
        let history: Vec<Option<Status>> = past
            .iter()
            .rev()
            .map(|s| if *s == "GAP" { None } else { Some(status(s)) })
            .collect();
        let opts = &c["options"];
        let depth = opts
            .get("historyDepth")
            .and_then(Value::as_i64)
            .unwrap_or(10) as i32;
        let period = opts
            .get("stabilizationPeriod")
            .and_then(Value::as_i64)
            .unwrap_or(5) as u32;
        let got = test_flakiness(status(current), &history, depth, period);
        assert_eq!(got, c["result"].as_bool(), "{seq:?} {opts}");
        if let Some(expected) = c.get("withoutGap") {
            let no_gap: Vec<Option<Status>> =
                history.iter().filter(|h| h.is_some()).copied().collect();
            assert_eq!(
                test_flakiness(status(current), &no_gap, depth, period),
                expected.as_bool()
            );
        }
        if let Some(d) = c.get("detector").and_then(Value::as_bool) {
            let mut o = FlakyOptions {
                history_depth: depth,
                stabilization_period: period,
                include_passed_tests: false,
            };
            assert_eq!(is_flaky(status(current), &history, &o), d, "{seq:?}");
            o.include_passed_tests = true;
            assert_eq!(
                is_flaky(status(current), &history, &o),
                c["detectorIncludePassed"].as_bool().unwrap(),
                "{seq:?}"
            );
        }
    }
}

#[test]
fn transition_vectors() {
    let v = vectors();
    for c in v["transitions"].as_array().unwrap() {
        let hist: Vec<Status> = c["historyNewestFirst"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| status(s.as_str().unwrap()))
            .collect();
        let got = status_transition(status(c["current"].as_str().unwrap()), &hist)
            .map(|t| serde_json::to_value(t).unwrap());
        assert_eq!(
            got.as_ref().and_then(Value::as_str),
            c["transition"].as_str(),
            "{c}"
        );
    }
}
