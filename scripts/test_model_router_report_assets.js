const nodes = [];

class FakeNode {
  constructor(name) {
    this.name = name;
    this.attributes = {};
    this.children = [];
    this.classList = { add: () => {} };
  }

  append(...children) {
    this.children.push(...children);
  }

  replaceChildren(...children) {
    this.children = children;
  }

  setAttribute(name, value) {
    this.attributes[name] = value;
  }
}

const root = new FakeNode("root");
const status = new FakeNode("status");

globalThis.document = {
  createElement: (name) => {
    const node = new FakeNode(name);
    nodes.push(node);
    return node;
  },
  createElementNS: (_namespace, name) => {
    const node = new FakeNode(name);
    nodes.push(node);
    return node;
  },
  createTextNode: (text) => new FakeNode(text),
  querySelector: (selector) => selector === "#report" ? root : status,
  title: "",
};
globalThis.location = { hash: "#capability=test-capability" };
globalThis.fetch = () => Promise.resolve({
  ok: true,
  json: () => Promise.resolve({
    document: {
      title: "Model router report",
      sections: [{
        kind: "lineChart",
        title: "Costs over time",
        xAxis: "Time (days)",
        yAxis: "Cost (USD)",
        series: [
          {
            label: "Actual cost",
            color: "blue",
            points: [
              { x: "2026-09-13", value: 0 },
              { x: "2026-09-14", value: null },
              { x: "2026-09-15", value: 254.05 },
              { x: "2026-09-16", value: null },
              { x: "2026-09-17", value: 0 },
            ],
          },
          {
            label: "Estimated baseline (priced usage)",
            color: "orange",
            points: [
              { x: "2026-09-13", value: 0 },
              { x: "2026-09-14", value: null },
              { x: "2026-09-15", value: 436.37 },
              { x: "2026-09-16", value: null },
              { x: "2026-09-17", value: 0 },
            ],
          },
          {
            label: "Estimated savings (priced usage)",
            color: "green",
            points: [
              { x: "2026-09-13", value: 0 },
              { x: "2026-09-14", value: null },
              { x: "2026-09-15", value: 182.32 },
              { x: "2026-09-16", value: null },
              { x: "2026-09-17", value: 0 },
            ],
          },
        ],
      }],
    },
  }),
});

await import(new URL("../xedoc-rs/app-server/src/model_router_report_assets/report.js", import.meta.url));
await new Promise((resolve) => setImmediate(resolve));

const markers = nodes.filter((node) => node.name === "circle");
if (markers.length !== 3) {
  throw new Error(`expected three visible isolated-point markers, found ${markers.length}`);
}
if (markers.some((marker) => Number(marker.attributes.cy) <= 0)) {
  throw new Error("expected visible markers within the chart area");
}
