const capability = new URLSearchParams(location.hash.slice(1)).get("capability");
const status = document.querySelector("#status");
const currency = new Intl.NumberFormat(undefined, { style: "currency", currency: "USD" });
const number = new Intl.NumberFormat();
const cell = (value) => {
  const element = document.createElement("td");
  element.textContent = value ?? "—";
  return element;
};
const row = (values) => {
  const element = document.createElement("tr");
  values.forEach((value) => element.append(cell(value)));
  return element;
};
const total = (days, key) => days.reduce((sum, day) => sum + (day[key] ?? 0), 0);
const money = (value) => value == null ? "unknown" : currency.format(value);
const route = (item) => [item.providerId, item.modelSlug, item.reasoningEffort].filter(Boolean).join("/");
const decisionRoute = (decision) => [
  decision.effectiveProviderId, decision.effectiveModelSlug, decision.effectiveReasoningEffort,
].filter(Boolean).join("/") || "not routed";
const confidence = (decision) => decision.classId
  ? `${decision.classId} ${decision.score == null ? "unknown" : decision.score.toFixed(2)} (${decision.margin == null ? "unknown" : `+${decision.margin.toFixed(2)}`})`
  : "not classified";
const addMetric = (label, value) => {
  const metric = document.createElement("div");
  metric.className = "metric";
  const title = document.createElement("strong");
  title.textContent = label;
  const result = document.createElement("span");
  result.textContent = value;
  metric.append(title, result);
  document.querySelector("#summary").append(metric);
};

if (!capability) {
  status.textContent = "This report link is missing its capability.";
} else {
  fetch("/api/report", { headers: { "X-Xedoc-Report-Capability": capability } })
    .then((response) => response.ok ? response.json() : Promise.reject(response.status))
    .then((report) => {
      status.textContent = `Showing ${report.fromDay} through ${report.throughDay}.`;
      const actual = total(report.days, "totalCostUsd");
      const baseline = total(report.days, "normalizedBaselineUsd");
      const savings = total(report.days, "estimatedSavingsUsd");
      const overhead = total(report.days, "abExperimentOverheadUsd");
      const tokens = total(report.days, "inputTokens") + total(report.days, "cachedInputTokens") + total(report.days, "outputTokens");
      addMetric("Actual cost", money(actual));
      addMetric("Baseline cost", money(baseline));
      addMetric("Estimated savings", money(savings));
      addMetric("A/B overhead", money(overhead));
      addMetric("Observed tokens", number.format(tokens));
      const invocations = total(report.days, "invocations");
      const unknownUsage = total(report.days, "missingUsageInvocations");
      const unknownPrice = total(report.days, "unknownPriceInvocations");
      document.querySelector("#coverage").textContent =
        `${number.format(invocations)} invocations; ${number.format(unknownUsage)} missing usage and ${number.format(unknownPrice)} with unknown pricing.`;
      if (report.daysTruncated) {
        const truncated = document.querySelector("#truncated");
        truncated.hidden = false;
        truncated.textContent = "Daily rows were truncated to the report limit.";
      }
      const days = document.querySelector("#days");
      report.days.forEach((day) => days.append(row([
        new Date(day.day * 1000).toISOString().slice(0, 10),
        route(day), day.scope,
        number.format((day.inputTokens ?? 0) + (day.cachedInputTokens ?? 0) + (day.outputTokens ?? 0)),
        money(day.totalCostUsd), money(day.normalizedBaselineUsd), money(day.estimatedSavingsUsd),
        money(day.abExperimentOverheadUsd),
      ])));
      const decisions = document.querySelector("#decisions");
      report.recentDecisions.forEach((decision) => decisions.append(row([
        new Date(decision.createdAt * 1000).toISOString(), decision.scope, decisionRoute(decision),
        confidence(decision), decision.fallback ? "yes" : "no", decision.reason,
      ])));
    })
    .catch(() => { status.textContent = "The report is unavailable or its capability expired."; });
}
