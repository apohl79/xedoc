const capability = new URLSearchParams(location.hash.slice(1)).get("capability");
const root = document.querySelector("#report");
const status = document.querySelector("#status");
const svgNamespace = "http://www.w3.org/2000/svg";
const palette = Object.freeze({
  blue: "#63b3ed",
  orange: "#f6ad55",
  green: "#68d391",
  purple: "#b794f4",
  red: "#fc8181",
  teal: "#4fd1c5",
});
const colorClasses = Object.freeze({
  blue: "color-blue",
  green: "color-green",
  orange: "color-orange",
  purple: "color-purple",
  red: "color-red",
  teal: "color-teal",
});

const element = (name, text) => {
  const node = document.createElement(name);
  if (text !== undefined) node.textContent = text;
  return node;
};

const svgElement = (name, attributes = {}) => {
  const node = document.createElementNS(svgNamespace, name);
  Object.entries(attributes).forEach(([key, value]) => node.setAttribute(key, String(value)));
  return node;
};

const appendTitle = (container, title) => {
  if (title) container.append(element("h2", title));
};

const renderMetricGrid = (section) => {
  const container = element("section");
  container.className = "section";
  appendTitle(container, section.title);
  const grid = element("div");
  grid.className = "metric-grid";
  section.metrics.forEach((metric) => {
    const card = element("div");
    card.className = "metric";
    const label = element("strong", metric.label);
    label.className = "metric-label";
    const value = element("span", metric.value);
    value.className = "metric-value";
    card.append(label, value);
    grid.append(card);
  });
  container.append(grid);
  return container;
};

const chartPath = (points, x, y) => points.reduce(
  ({ path, started }, point, index) => point.value == null
    ? { path, started: false }
    : {
      path: `${path}${started ? "L" : "M"}${x(index).toFixed(2)} ${y(point.value).toFixed(2)} `,
      started: true,
    },
  { path: "", started: false },
).path;

const renderLineChart = (section) => {
  const container = element("section");
  container.className = "section chart";
  appendTitle(container, section.title);

  const legend = element("div");
  legend.className = "legend";
  section.series.forEach((series) => {
    const item = element("span");
    item.className = "legend-item";
    const swatch = element("span");
    swatch.className = "legend-swatch";
    swatch.classList.add(colorClasses[series.color]);
    item.append(swatch, document.createTextNode(series.label));
    legend.append(item);
  });

  const width = 760;
  const height = 330;
  const padding = { top: 24, right: 24, bottom: 52, left: 86 };
  const chartWidth = width - padding.left - padding.right;
  const chartHeight = height - padding.top - padding.bottom;
  const values = section.series.flatMap((series) => series.points
    .map((point) => point.value)
    .filter((value) => value != null));
  const minimum = Math.min(0, ...values);
  const maximum = Math.max(0, ...values);
  const range = maximum - minimum || 1;
  const pointCount = section.series[0]?.points.length ?? 0;
  const x = (index) => padding.left + (chartWidth * index) / Math.max(pointCount - 1, 1);
  const y = (value) => padding.top + ((maximum - value) * chartHeight) / range;
  const svg = svgElement("svg", {
    viewBox: `0 0 ${width} ${height}`,
    role: "img",
    "aria-label": section.title,
  });
  const svgTitle = svgElement("title");
  svgTitle.textContent = section.title;
  svg.append(svgTitle);

  Array.from({ length: 5 }, (_, index) => minimum + (range * index) / 4).forEach((value) => {
    const lineY = y(value);
    const gridLine = svgElement("line", {
      x1: padding.left, x2: width - padding.right, y1: lineY, y2: lineY,
    });
    gridLine.classList.add("grid-line");
    svg.append(gridLine);
    const label = svgElement("text", {
      x: padding.left - 8, y: lineY + 4, "text-anchor": "end",
    });
    label.textContent = String(Math.round(value * 100) / 100);
    svg.append(label);
  });

  const horizontalAxis = svgElement("line", {
    x1: padding.left, x2: width - padding.right, y1: y(0), y2: y(0),
  });
  horizontalAxis.classList.add("axis");
  const verticalAxis = svgElement("line", {
    x1: padding.left, x2: padding.left, y1: padding.top, y2: height - padding.bottom,
  });
  verticalAxis.classList.add("axis");
  svg.append(horizontalAxis, verticalAxis);

  const points = section.series[0]?.points ?? [];
  const labelIndexes = points.length <= 3
    ? points.map((_, index) => index)
    : [0, Math.floor((points.length - 1) / 2), points.length - 1];
  labelIndexes.forEach((index) => {
    const label = svgElement("text", {
      x: x(index), y: height - 28, "text-anchor": "middle",
    });
    label.textContent = points[index].x;
    svg.append(label);
  });

  const verticalLabel = svgElement("text", {
    x: 18,
    y: padding.top + chartHeight / 2,
    transform: `rotate(-90 18 ${padding.top + chartHeight / 2})`,
    "text-anchor": "middle",
  });
  verticalLabel.textContent = section.yAxis;
  const horizontalLabel = svgElement("text", {
    x: padding.left + chartWidth / 2, y: height - 6, "text-anchor": "middle",
  });
  horizontalLabel.textContent = section.xAxis;
  svg.append(verticalLabel, horizontalLabel);

  section.series.forEach((series) => {
    const path = svgElement("path", {
      d: chartPath(series.points, x, y),
      stroke: palette[series.color],
    });
    path.classList.add("series");
    svg.append(path);
  });
  container.append(legend, svg);
  return container;
};

const renderTable = (section) => {
  const container = element("section");
  container.className = "section";
  appendTitle(container, section.title);
  const scroll = element("div");
  scroll.className = "table-scroll";
  const table = element("table");
  const head = element("thead");
  const headingRow = element("tr");
  section.columns.forEach((column) => headingRow.append(element("th", column)));
  head.append(headingRow);
  const body = element("tbody");
  section.rows.forEach((values) => {
    const row = element("tr");
    values.forEach((value) => row.append(element("td", value)));
    body.append(row);
  });
  table.append(head, body);
  scroll.append(table);
  container.append(scroll);
  return container;
};

const renderNotice = (section) => {
  const notice = element("p", section.text);
  notice.className = "section notice";
  if (section.level === "warning") notice.classList.add("notice-warning");
  if (section.level === "error") notice.classList.add("notice-error");
  return notice;
};

const renderDocument = (reportDocument) => {
  document.title = reportDocument.title;
  const heading = element("h1", reportDocument.title);
  const sections = reportDocument.sections.map((section) => {
    switch (section.kind) {
      case "metricGrid": return renderMetricGrid(section);
      case "lineChart": return renderLineChart(section);
      case "table": return renderTable(section);
      case "notice": return renderNotice(section);
      default: throw new Error("unsupported report section");
    }
  });
  status.textContent = "";
  root.replaceChildren(heading, ...sections);
};

if (!capability) {
  status.textContent = "This report link is missing its capability.";
} else {
  fetch("/api/report", {
    headers: { "X-Xedoc-Report-Capability": capability },
  })
    .then((response) => response.ok ? response.json() : Promise.reject(response.status))
    .then(({ document: reportDocument }) => renderDocument(reportDocument))
    .catch(() => {
      status.textContent = "The report is unavailable or its capability expired.";
    });
}
