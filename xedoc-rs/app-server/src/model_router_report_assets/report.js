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
const completeTotal = (report, key) => report.daysTruncated
  || report.days.some((day) => day.invocations > 0 && day[key] == null)
  ? null
  : total(report.days, key);
const money = (value) => value == null ? "unknown" : currency.format(value);
const savingsRate = (savings, baseline) => savings == null || baseline == null || baseline === 0
  ? "unknown"
  : `${((savings / baseline) * 100).toFixed(1)}%`;
const route = (item) => [item.providerId, item.modelSlug, item.reasoningEffort].filter(Boolean).join("/");
const graphMetrics = [
  { key: "actual", source: "totalCostUsd", label: "Actual cost", className: "actual" },
  {
    key: "baseline",
    source: "normalizedBaselineUsd",
    label: "Estimated baseline (price-normalized)",
    className: "baseline",
  },
  { key: "savings", source: "estimatedSavingsUsd", label: "Estimated savings", className: "savings" },
];
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
const svgElement = (name, attributes = {}) => {
  const element = document.createElementNS("http://www.w3.org/2000/svg", name);
  Object.entries(attributes).forEach(([key, value]) => element.setAttribute(key, value));
  return element;
};
const graphDays = (report) => {
  const grouped = new Map();
  const incompleteFromDay = report.daysTruncated ? report.days.at(-1)?.day : undefined;
  for (let day = report.fromDay; day <= report.throughDay; day += 86_400) {
    grouped.set(day, Object.fromEntries(graphMetrics.map(({ key }) => [
      key, { complete: incompleteFromDay == null || day < incompleteFromDay, value: 0 },
    ])));
  }
  report.days.forEach((record) => {
    const costs = grouped.get(record.day);
    if (!costs || record.invocations === 0) return;
    graphMetrics.forEach(({ key, source }) => {
      if (record[source] == null) {
        costs[key].complete = false;
      } else {
        costs[key].value += record[source];
      }
    });
  });
  return [...grouped].map(([day, costs]) => ({
    day,
    ...Object.fromEntries(graphMetrics.map(({ key }) => [
      key, costs[key].complete ? costs[key].value : null,
    ])),
  }));
};
const graphPath = (days, key, x, y) => days.reduce(
  ({ path, started }, day, index) => day[key] == null
    ? { path, started: false }
    : {
      path: `${path}${started ? "L" : "M"}${x(index).toFixed(2)} ${y(day[key]).toFixed(2)} `,
      started: true,
    },
  { path: "", started: false },
).path;
const renderCostGraph = (report) => {
  const container = document.querySelector("#cost-graph");
  const days = graphDays(report);
  const values = days.flatMap((day) => graphMetrics.map(({ key }) => day[key]).filter((value) => value != null));
  if (values.length === 0) {
    container.textContent = "Cost data is unavailable for the selected days.";
    return;
  }
  const width = 760;
  const height = 330;
  const padding = { top: 24, right: 24, bottom: 52, left: 86 };
  const chartWidth = width - padding.left - padding.right;
  const chartHeight = height - padding.top - padding.bottom;
  const minimum = Math.min(0, ...values);
  const maximum = Math.max(0, ...values);
  const range = maximum - minimum || 1;
  const x = (index) => padding.left + (chartWidth * index) / Math.max(days.length - 1, 1);
  const y = (value) => padding.top + ((maximum - value) * chartHeight) / range;
  const legend = document.createElement("div");
  legend.className = "legend";
  graphMetrics.forEach(({ label, className }) => {
    const item = document.createElement("span");
    item.className = "legend-item";
    const swatch = document.createElement("span");
    swatch.className = `legend-swatch ${className}`;
    item.append(swatch, document.createTextNode(label));
    legend.append(item);
  });
  const svg = svgElement("svg", {
    viewBox: `0 0 ${width} ${height}`,
    role: "img",
    "aria-label": "Daily actual cost, estimated baseline cost, and estimated savings",
  });
  const title = svgElement("title");
  title.textContent = "Costs over time";
  svg.append(title);
  Array.from({ length: 5 }, (_, index) => minimum + (range * index) / 4).forEach((value) => {
    const lineY = y(value);
    svg.append(svgElement("line", {
      class: "grid", x1: padding.left, x2: width - padding.right, y1: lineY, y2: lineY,
    }));
    const label = svgElement("text", { x: padding.left - 8, y: lineY + 4, "text-anchor": "end" });
    label.textContent = money(value);
    svg.append(label);
  });
  svg.append(svgElement("line", {
    class: "axis", x1: padding.left, x2: width - padding.right, y1: y(0), y2: y(0),
  }));
  svg.append(svgElement("line", {
    class: "axis", x1: padding.left, x2: padding.left, y1: padding.top, y2: height - padding.bottom,
  }));
  const labelIndexes = days.length <= 3 ? days.map((_, index) => index) : [0, Math.floor((days.length - 1) / 2), days.length - 1];
  labelIndexes.forEach((index) => {
    const label = svgElement("text", { x: x(index), y: height - 28, "text-anchor": "middle" });
    label.textContent = new Date(days[index].day * 1000).toISOString().slice(0, 10);
    svg.append(label);
  });
  const verticalLabel = svgElement("text", {
    x: 18, y: padding.top + chartHeight / 2, transform: `rotate(-90 18 ${padding.top + chartHeight / 2})`,
    "text-anchor": "middle",
  });
  verticalLabel.textContent = "Cost (USD)";
  svg.append(verticalLabel);
  const horizontalLabel = svgElement("text", {
    x: padding.left + chartWidth / 2, y: height - 6, "text-anchor": "middle",
  });
  horizontalLabel.textContent = "Time (days)";
  svg.append(horizontalLabel);
  graphMetrics.forEach(({ key, className }) => svg.append(svgElement("path", {
    class: `series ${className}`, d: graphPath(days, key, x, y),
  })));
  container.replaceChildren(legend, svg);
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
      const completeBaseline = completeTotal(report, "normalizedBaselineUsd");
      const completeSavings = completeTotal(report, "estimatedSavingsUsd");
      const overhead = total(report.days, "abExperimentOverheadUsd");
      const tokens = total(report.days, "inputTokens") + total(report.days, "cachedInputTokens") + total(report.days, "outputTokens");
      addMetric("Actual cost", money(actual));
      addMetric("Baseline cost", money(baseline));
      addMetric("Estimated savings", money(savings));
      addMetric("Estimated savings rate", savingsRate(completeSavings, completeBaseline));
      addMetric("A/B overhead", money(overhead));
      addMetric("Observed tokens", number.format(tokens));
      renderCostGraph(report);
      const invocations = total(report.days, "invocations");
      const unknownUsage = total(report.days, "missingUsageInvocations");
      const unknownPrice = total(report.days, "unknownPriceInvocations");
      const unknownBaselinePrice = total(report.days, "unknownBaselinePriceInvocations");
      document.querySelector("#coverage").textContent =
        `${number.format(invocations)} invocations; ${number.format(unknownUsage)} missing usage, ` +
        `${number.format(unknownPrice)} with unknown actual pricing, and ` +
        `${number.format(unknownBaselinePrice)} without baseline pricing.`;
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
        savingsRate(day.estimatedSavingsUsd, day.normalizedBaselineUsd),
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
