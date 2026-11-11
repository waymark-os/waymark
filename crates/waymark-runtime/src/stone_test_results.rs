//! Test results as values: reads the output of pytest, Django's
//! `tests/runtests.py`, and SymPy's `bin/test` into per-test outcomes and the
//! runner's own counts, for `run_tests()`.
//!
//! A run is `complete` only when the runner's final summary line was found.
//! An unparsed run (a crash before the summary, a timeout, a wrong command)
//! is never read as passing: `run_tests()` reports `ok = False` for it.

/// Per-test outcomes and counts read from one test run's output.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct TestOutcomes {
    pub passed_tests: Vec<String>,
    pub failed_tests: Vec<String>,
    pub skipped_tests: Vec<String>,
    pub passed: i64,
    /// Failures and errors together.
    pub failed: i64,
    pub skipped: i64,
    pub xfailed: i64,
    /// The runner's final summary was found.
    pub complete: bool,
}

pub(crate) const RUNNERS: &[&str] = &["pytest", "django", "sympy"];

pub(crate) fn parse(runner: &str, output: &str) -> TestOutcomes {
    let mut outcomes = match runner {
        "django" => parse_django(output),
        "sympy" => parse_sympy(output),
        _ => parse_pytest(output),
    };
    if !outcomes.complete {
        // Without the runner's summary the counts come from the names seen.
        outcomes.passed = outcomes.passed_tests.len() as i64;
        outcomes.failed = outcomes.failed_tests.len() as i64;
        outcomes.skipped = outcomes.skipped_tests.len() as i64;
    }
    outcomes
}

fn push_unique(list: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if !name.is_empty() && !list.iter().any(|seen| seen == name) {
        list.push(name.to_string());
    }
}

/// `3 failed, 2 passed, 1 skipped` style counts; returns the pairs found.
fn count_pairs(text: &str) -> Vec<(i64, String)> {
    let mut pairs = Vec::new();
    for part in text.split(',') {
        let mut words = part.split_whitespace();
        let Some(number) = words.next().and_then(|word| word.parse::<i64>().ok()) else {
            continue;
        };
        let label = words.collect::<Vec<_>>().join(" ");
        if !label.is_empty() {
            pairs.push((number, label));
        }
    }
    pairs
}

// pytest -------------------------------------------------------------------

const PYTEST_STATUSES: &[&str] = &["PASSED", "FAILED", "ERROR", "SKIPPED", "XFAIL", "XPASS"];

fn parse_pytest(output: &str) -> TestOutcomes {
    let mut outcomes = TestOutcomes::default();
    for line in output.lines() {
        let text = line.trim();
        // Short summary (`-rA`, or the default `-rfE`): `FAILED id - message`.
        if let Some((status, rest)) = text.split_once(' ') {
            if PYTEST_STATUSES.contains(&status) && status != "SKIPPED" {
                let id = rest.split(" - ").next().unwrap_or(rest);
                if id.contains("::") || id.ends_with(".py") {
                    record_pytest(&mut outcomes, status, id);
                    continue;
                }
            }
            if status == "SKIPPED" {
                // `SKIPPED [1] path:line: reason` names no test id.
                if rest.contains("::") {
                    record_pytest(&mut outcomes, status, rest.split(" - ").next().unwrap_or(rest));
                }
                continue;
            }
        }
        // Verbose progress (`-v`): `path::test PASSED [ 50%]`.
        let mut words = text.split_whitespace();
        if let (Some(id), Some(status)) = (words.next(), words.next()) {
            if id.contains("::") && PYTEST_STATUSES.contains(&status) {
                record_pytest(&mut outcomes, status, id);
                continue;
            }
        }
        if let Some(counts) = pytest_summary(text) {
            outcomes.complete = true;
            outcomes.passed = 0;
            outcomes.failed = 0;
            outcomes.skipped = 0;
            outcomes.xfailed = 0;
            for (number, label) in counts {
                match label.as_str() {
                    "passed" | "xpassed" => outcomes.passed += number,
                    "failed" | "error" | "errors" => outcomes.failed += number,
                    "skipped" => outcomes.skipped += number,
                    "xfailed" => outcomes.xfailed += number,
                    _ => {}
                }
            }
        }
    }
    outcomes
}

fn record_pytest(outcomes: &mut TestOutcomes, status: &str, id: &str) {
    match status {
        "PASSED" | "XPASS" => push_unique(&mut outcomes.passed_tests, id),
        "FAILED" | "ERROR" => push_unique(&mut outcomes.failed_tests, id),
        "SKIPPED" => push_unique(&mut outcomes.skipped_tests, id),
        _ => {}
    }
}

/// The final line, `3 failed, 2 passed in 0.04s` or `no tests ran in 0.01s`,
/// possibly framed by `=` signs.
fn pytest_summary(text: &str) -> Option<Vec<(i64, String)>> {
    let inner = text.trim_matches(|c: char| c == '=' || c.is_whitespace());
    let (counts, time) = inner.rsplit_once(" in ")?;
    // `in 0.04s`, or `in 61.20s (0:01:01)` on long runs.
    let time = time.split_whitespace().next()?;
    if !(time.ends_with('s') && time.starts_with(|c: char| c.is_ascii_digit())) {
        return None;
    }
    if counts == "no tests ran" {
        return Some(Vec::new());
    }
    let pairs = count_pairs(counts);
    let known = [
        "passed", "failed", "error", "errors", "skipped", "xfailed", "xpassed", "deselected",
        "warning", "warnings", "rerun",
    ];
    if pairs.is_empty()
        || pairs.len() != counts.split(',').count()
        || !pairs.iter().all(|(_, label)| known.contains(&label.as_str()))
    {
        return None;
    }
    Some(pairs)
}

// Django -------------------------------------------------------------------

fn django_id(text: &str) -> Option<&str> {
    // `test_name (dotted.path)` or `test_name (dotted.path.test_name)`.
    let (name, rest) = text.split_once(" (")?;
    let path = rest.strip_suffix(')')?;
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    (plain(name) && plain(path)).then_some(text)
}

fn parse_django(output: &str) -> TestOutcomes {
    let mut outcomes = TestOutcomes::default();
    let mut pending: Option<String> = None;
    let mut ran = false;
    for line in output.lines() {
        let text = line.trim_end();
        if let Some((head, status)) = text.rsplit_once(" ... ") {
            // A docstring puts the status on the line after the id.
            let id = django_id(head.trim()).map(str::to_string).or_else(|| pending.take());
            pending = None;
            if let Some(id) = id {
                let status = status.trim();
                if status == "ok" {
                    push_unique(&mut outcomes.passed_tests, &id);
                } else if status == "FAIL" || status == "ERROR" || status == "unexpected success" {
                    push_unique(&mut outcomes.failed_tests, &id);
                } else if status.starts_with("skipped") {
                    push_unique(&mut outcomes.skipped_tests, &id);
                }
            }
            continue;
        }
        if let Some(id) = text.strip_prefix("FAIL: ").or_else(|| text.strip_prefix("ERROR: ")) {
            if django_id(id.trim()).is_some() {
                push_unique(&mut outcomes.failed_tests, id);
            }
            continue;
        }
        if let Some(id) = django_id(text.trim()) {
            pending = Some(id.to_string());
            continue;
        }
        pending = None;
        if let Some(rest) = text.strip_prefix("Ran ") {
            if let Some((number, _)) = rest.split_once(" test") {
                if let Ok(number) = number.parse::<i64>() {
                    ran = true;
                    outcomes.passed = number;
                }
            }
            continue;
        }
        if ran && (text == "OK" || text.starts_with("OK (") || text.starts_with("FAILED (")) {
            outcomes.complete = true;
            let details = text
                .split_once('(')
                .map(|(_, rest)| rest.trim_end_matches(')'))
                .unwrap_or("");
            let mut total = outcomes.passed;
            for part in details.split(',') {
                let Some((label, number)) = part.trim().split_once('=') else {
                    continue;
                };
                let Ok(number) = number.parse::<i64>() else {
                    continue;
                };
                match label {
                    "failures" | "errors" | "unexpected successes" => outcomes.failed += number,
                    "skipped" => outcomes.skipped += number,
                    "expected failures" => outcomes.xfailed += number,
                    _ => {}
                }
            }
            total -= outcomes.failed + outcomes.skipped + outcomes.xfailed;
            outcomes.passed = total.max(0);
        }
    }
    outcomes
}

// SymPy --------------------------------------------------------------------

fn parse_sympy(output: &str) -> TestOutcomes {
    let mut outcomes = TestOutcomes::default();
    let mut file: Option<String> = None;
    for line in output.lines() {
        let text = line
            .trim_end()
            .trim_end_matches("[OK]")
            .trim_end_matches("[FAIL]")
            .trim_end();
        // `sympy/core/tests/test_x.py[5] ` opens a file's results.
        if let Some((path, rest)) = text.trim().split_once(".py[") {
            if rest.trim_end().trim_end_matches(|c: char| c != ']').ends_with(']')
                && !path.contains(' ')
            {
                file = Some(format!("{path}.py"));
                continue;
            }
        }
        // `___ path.py:test_name ___` heads a failure's traceback.
        if text.starts_with("___") && text.ends_with("___") {
            let inner = text.trim_matches(|c: char| c == '_' || c.is_whitespace());
            if let Some((path, name)) = inner.rsplit_once(".py:") {
                push_unique(&mut outcomes.failed_tests, &format!("{path}.py::{name}"));
            }
            continue;
        }
        if let Some(file) = file.as_ref() {
            let mut words = text.split_whitespace();
            let (Some(name), Some(status)) = (words.next(), text.split_whitespace().last()) else {
                continue;
            };
            if !name.starts_with("test") || words.next().is_none() {
                continue;
            }
            let id = format!("{file}::{name}");
            match status {
                "ok" | "X" => push_unique(&mut outcomes.passed_tests, &id),
                "F" | "E" | "T" => push_unique(&mut outcomes.failed_tests, &id),
                "s" | "w" => push_unique(&mut outcomes.skipped_tests, &id),
                _ => {}
            }
        }
    }
    // `tests finished: 1 passed, 1 failed, ..., \n1 exceptions, in 0.01 seconds`
    if let Some(start) = output.find("tests finished:") {
        let rest = &output[start + "tests finished:".len()..];
        if let Some(end) = rest.find(" seconds") {
            outcomes.complete = true;
            let summary = rest[..end].split_whitespace().collect::<Vec<_>>().join(" ");
            for (number, label) in count_pairs(&summary) {
                match label.as_str() {
                    "passed" | "expected to fail but passed" => outcomes.passed += number,
                    "failed" | "exceptions" | "exception" => outcomes.failed += number,
                    "skipped" => outcomes.skipped += number,
                    "expected to fail" => outcomes.xfailed += number,
                    _ => {}
                }
            }
        }
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;

    const PYTEST_FAIL: &str = ".FFxs.F                                                                  [100%]
=================================== FAILURES ===================================
__________________________________ test_fail ___________________________________

    def test_fail():
>       assert 1 == 2
E       assert 1 == 2

/tmp/test_zz.py:5: AssertionError
==================================== PASSES ====================================
=========================== short test summary info ============================
PASSED ../tmp/test_zz.py::test_ok
PASSED ../tmp/test_zz.py::test_p[1]
SKIPPED [1] test_zz.py:11: why
XFAIL ../tmp/test_zz.py::test_xf
FAILED ../tmp/test_zz.py::test_fail - assert 1 == 2
FAILED ../tmp/test_zz.py::test_err - ValueError: x
FAILED ../tmp/test_zz.py::test_p[2] - assert 2 == 1
3 failed, 2 passed, 1 skipped, 1 xfailed in 0.04s
";

    #[test]
    fn reads_pytest_short_summary_and_counts() {
        let out = parse("pytest", PYTEST_FAIL);
        assert!(out.complete);
        assert_eq!(out.passed_tests, ["../tmp/test_zz.py::test_ok", "../tmp/test_zz.py::test_p[1]"]);
        assert_eq!(
            out.failed_tests,
            [
                "../tmp/test_zz.py::test_fail",
                "../tmp/test_zz.py::test_err",
                "../tmp/test_zz.py::test_p[2]"
            ]
        );
        assert!(out.skipped_tests.is_empty());
        assert_eq!((out.passed, out.failed, out.skipped, out.xfailed), (2, 3, 1, 1));

        let ok = parse(
            "pytest",
            "PASSED testing/test_conftest.py::test_conftest_confcutdir\nSKIPPED [1] testing/test_conftest.py:280: only relevant\n7 passed, 1 skipped, 45 deselected in 0.16s\n",
        );
        assert!(ok.complete);
        assert_eq!((ok.passed, ok.failed, ok.skipped), (7, 0, 1));

        let verbose = parse(
            "pytest",
            "t.py::test_a PASSED [ 50%]\nt.py::test_b ERROR [100%]\n==== 1 passed, 1 error in 0.10s ====\n",
        );
        assert_eq!(verbose.failed_tests, ["t.py::test_b"]);
        assert_eq!((verbose.passed, verbose.failed), (1, 1));

        let none = parse("pytest", "no tests ran in 0.01s\n");
        assert!(none.complete);
        assert_eq!(none.passed + none.failed, 0);
    }

    #[test]
    fn a_run_without_the_runner_summary_is_incomplete() {
        let crashed = parse(
            "pytest",
            "ImportError while loading conftest '/testbed/conftest.py'.\nE   ModuleNotFoundError: No module named 'x'\n",
        );
        assert!(!crashed.complete);
        assert_eq!(crashed.passed + crashed.failed, 0);

        let cut = parse("pytest", "PASSED t.py::test_a\nFAILED t.py::test_b - boom\n");
        assert!(!cut.complete);
        assert_eq!((cut.passed, cut.failed), (1, 1));

        assert!(!parse("django", "test_a (m.T.test_a) ... ok\n").complete);
        assert!(!parse("sympy", "sympy/x/tests/test_a.py[2] \ntest_a ok\n").complete);
    }

    #[test]
    fn reads_django_verbose_lines_and_final_status() {
        let failing = "Found 5 test(s).
test_err (zz_tests.tests.T.test_err) ... ERROR
test_fail (zz_tests.tests.T.test_fail) ... FAIL
test_ok (zz_tests.tests.T.test_ok) ... ok
test_sk (zz_tests.tests.T.test_sk) ... skipped 'why'
test_xf (zz_tests.tests.T.test_xf) ... expected failure

======================================================================
ERROR: test_err (zz_tests.tests.T.test_err)
----------------------------------------------------------------------
Traceback (most recent call last):
ValueError: x

----------------------------------------------------------------------
Ran 5 tests in 0.001s

FAILED (failures=1, errors=1, skipped=1, expected failures=1)
";
        let out = parse("django", failing);
        assert!(out.complete);
        assert_eq!(out.passed_tests, ["test_ok (zz_tests.tests.T.test_ok)"]);
        assert_eq!(
            out.failed_tests,
            ["test_err (zz_tests.tests.T.test_err)", "test_fail (zz_tests.tests.T.test_fail)"]
        );
        assert_eq!(out.skipped_tests, ["test_sk (zz_tests.tests.T.test_sk)"]);
        assert_eq!((out.passed, out.failed, out.skipped, out.xfailed), (1, 2, 1, 1));

        let docstring = "test_inputs (t.F.test_inputs) ... ok
test_low_decimal_precision (t.F.test_low_decimal_precision)
#15789 ... ok

----------------------------------------------------------------------
Ran 2 tests in 0.011s

OK
";
        let out = parse("django", docstring);
        assert!(out.complete);
        assert_eq!(
            out.passed_tests,
            ["test_inputs (t.F.test_inputs)", "test_low_decimal_precision (t.F.test_low_decimal_precision)"]
        );
        assert_eq!((out.passed, out.failed), (2, 0));

        // Verbosity 1 prints no per-test lines; failures still name their test.
        let quiet = parse(
            "django",
            "F.\n======\nFAIL: test_b (m.T.test_b)\n------\nRan 2 tests in 0.1s\n\nFAILED (failures=1)\n",
        );
        assert_eq!(quiet.failed_tests, ["test_b (m.T.test_b)"]);
        assert_eq!((quiet.passed, quiet.failed), (1, 1));
    }

    #[test]
    fn reads_sympy_verbose_lines_and_summary() {
        let failing = "sympy/core/tests/test_zz.py[5]
test_ok ok
test_fail F
test_err E
test_xf f
test_sk why s                                                             [FAIL]


________________________________________________________________________________
_____________________ sympy/core/tests/test_zz.py:test_err _____________________
Traceback (most recent call last):
ValueError: x

 tests finished: 1 passed, 1 failed, 1 skipped, 1 expected to fail,
1 exceptions, in 0.01 seconds
DO *NOT* COMMIT!
";
        let out = parse("sympy", failing);
        assert!(out.complete);
        assert_eq!(out.passed_tests, ["sympy/core/tests/test_zz.py::test_ok"]);
        assert_eq!(
            out.failed_tests,
            ["sympy/core/tests/test_zz.py::test_fail", "sympy/core/tests/test_zz.py::test_err"]
        );
        assert_eq!(out.skipped_tests, ["sympy/core/tests/test_zz.py::test_sk"]);
        assert_eq!((out.passed, out.failed, out.skipped, out.xfailed), (1, 2, 1, 1));

        let ok = parse(
            "sympy",
            "sympy/polys/matrices/tests/test_normalforms.py[2] \ntest_smith_normal ok\ntest_hermite_normal ok                                                      [OK]\n\n\n================== tests finished: 2 passed, in 0.11 seconds ===================\n",
        );
        assert!(ok.complete);
        assert_eq!(ok.passed_tests.len(), 2);
        assert_eq!((ok.passed, ok.failed), (2, 0));

        // Without -v only the failure headers name tests.
        let quiet = parse(
            "sympy",
            "sympy/a/tests/test_b.py[2] .F    [FAIL]\n\n___ sympy/a/tests/test_b.py:test_c ___\n tests finished: 1 passed, 1 failed, in 0.01 seconds\n",
        );
        assert_eq!(quiet.failed_tests, ["sympy/a/tests/test_b.py::test_c"]);
        assert_eq!((quiet.passed, quiet.failed), (1, 1));
    }
}
