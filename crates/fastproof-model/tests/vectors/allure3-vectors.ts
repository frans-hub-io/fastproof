import { calculateTestCaseHash, calculateParametersHash, calculateEnvironmentHash, calculateRetryHash, stringifyIdentityParameters } from "./identity.ts";
import { getTestFlakiness } from "./flaky.ts";
import { getStatusTransition } from "./new.ts";
const createFlakyDetector = ({ includePassedTests = false, historyDepth, stabilizationPeriod }: any = {}) => (tr: any, history: any[]): boolean => {
  if (tr.status !== "failed" && tr.status !== "broken" && !(includePassedTests && tr.status === "passed")) {
    return false;
  }
  return getTestFlakiness(tr, history, { historyDepth, stabilizationPeriod }) === true;
};

const idCases: any[] = [
  { name: "full name only", testCaseId: null, fullName: "tests.test_sample#test_pass", params: [] },
  { name: "testCaseId wins", testCaseId: "a1b2c3", fullName: "tests.test_sample#test_pass", params: [] },
  { name: "empty testCaseId falls back", testCaseId: "", fullName: "com.example.CalcTest.adds", params: [] },
  { name: "unicode NFC full name", testCaseId: null, fullName: "suite › Привіт café", params: [{ name: "lang", value: "uk" }] },
  { name: "sorted by utf8 and dedup", testCaseId: null, fullName: "p#t", params: [
    { name: "b", value: "2" }, { name: "a", value: "1" }, { name: "a", value: "1" }, { name: "Z", value: "z" }, { name: "é", value: "x" }, { name: "a", value: "0" } ] },
  { name: "excluded, missing value, empty name", testCaseId: null, fullName: "p#t", params: [
    { name: "user", value: "alice" }, { name: "ts", value: "123", excluded: true }, { name: "opt" }, { name: "opt2", value: null }, { name: "", value: "ignored" } ] },
  { name: "masked/hidden still counted", testCaseId: null, fullName: "p#t", params: [
    { name: "password", value: "s3cret", mode: "masked" }, { name: "token", value: "t0k", mode: "hidden" } ] },
  { name: "astral vs BMP ordering", testCaseId: null, fullName: "p#t", params: [
    { name: "k", value: "｡" }, { name: "k", value: "\u{1F600}" } ] },
  { name: "env hash", testCaseId: null, fullName: "tests/login.spec.ts > login > works", params: [{ name: "browser", value: "chromium" }], env: "staging" },
  { name: "default env ignored", testCaseId: null, fullName: "tests/login.spec.ts > login > works", params: [], env: "default" },
];
const ids = idCases.map((c) => {
  const testCaseHash = calculateTestCaseHash(c.testCaseId, c.fullName);
  const parametersString = stringifyIdentityParameters(c.params);
  const parametersHash = calculateParametersHash(c.params);
  const environmentHash = calculateEnvironmentHash(c.env);
  const retryHash = calculateRetryHash({ testCaseHash, parametersHash, environmentHash });
  return { ...c, testCaseHash, parametersString, parametersHash, environmentHash, retryHash };
});

const seqs: string[][] = [
  ["passed"], ["passed","passed"], ["failed","passed"], ["passed","failed","passed"], ["passed","failed","passed","failed"],
  ["failed","failed","failed","failed","failed","passed","failed"], ["passed","passed","passed","passed","passed","failed","passed"],
  ["passed","broken","passed"], ["failed","passed","failed"], ["skipped","passed","failed","passed"],
  ["passed","failed","passed","failed","passed","failed","passed","failed","passed","failed","passed","passed","passed","passed","passed","passed"],
  ["broken","failed","broken"], ["passed","unknown","failed","passed"], ["failed","passed","failed","passed","failed","passed","passed","passed","passed","passed","failed"],
];
const flaky: any[] = [];
for (const s of seqs) {
  for (const opts of [{}, { historyDepth: 3, stabilizationPeriod: 2 }, { historyDepth: 0 }, { historyDepth: -1 }]) {
    const current = { id: "cur", status: s[s.length - 1] } as any;
    const history = s.slice(0, -1).reverse().map((st, i) => ({ id: "h" + i, status: st })) as any;
    flaky.push({ chronological: s, options: opts, result: getTestFlakiness(current, history, opts) ?? null, detector: createFlakyDetector(opts)(current, history), detectorIncludePassed: createFlakyDetector({ ...opts, includePassedTests: true })(current, history) });
  }
}
const gap = { chronological: ["passed","failed","GAP","passed","failed","passed"], note: "GAP=undefined in history" } as any;
{
  const history = [{id:"a",status:"failed"},{id:"b",status:"passed"},undefined,{id:"c",status:"failed"},{id:"d",status:"passed"}] as any;
  gap.result = getTestFlakiness({ id: "cur", status: "passed" } as any, history, {}) ?? null;
  const history2 = [{id:"a",status:"failed"},{id:"b",status:"passed"},{id:"c",status:"failed"},{id:"d",status:"passed"}] as any;
  gap.withoutGap = getTestFlakiness({ id: "cur", status: "passed" } as any, history2, {}) ?? null;
}
flaky.push(gap);

const tr: any[] = [];
for (const [cur, hist] of [["passed", []], ["passed", ["failed"]], ["failed", ["passed"]], ["broken", ["passed"]], ["broken", ["broken"]], ["skipped", ["passed"]], ["passed", ["skipped","failed"]], ["failed", ["unknown","skipped"]], ["passed", ["skipped"]]] as any) {
  tr.push({ current: cur, historyNewestFirst: hist, transition: getStatusTransition({ status: cur } as any, hist.map((s: string) => ({ status: s }))) ?? null });
}
console.log(JSON.stringify({ source: "allure3 v3.33.0 packages/core-api/src/utils/{identity,flaky}.ts, packages/core/src/utils/new.ts", identity: ids, flaky, transitions: tr }, null, 2));
