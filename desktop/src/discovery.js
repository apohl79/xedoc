"use strict";

// Port of HostDiscovery.swift: nearby hosts over mDNS (_xedoc-gui._tcp) plus reachable paired hosts.
const { EventEmitter } = require("node:events");
const { Bonjour } = require("bonjour-service");
const pairing = require("./pairing");
const { validatedHostURL } = require("./urls");

class HostDiscovery extends EventEmitter {
  constructor() {
    super();
    this.hosts = [];
    this.scanning = false;
    this.status = "Searching local network…";
    this.generation = 0;
    this.nearbyHosts = [];
    this.pairedHosts = [];
    this.timers = [];
  }

  snapshot() {
    return { hosts: this.hosts, scanning: this.scanning, status: this.status };
  }

  publish() {
    this.emit("change", this.snapshot());
  }

  stop() {
    this.generation += 1;
    this.timers.forEach(clearTimeout);
    this.timers = [];
    this.browser?.stop();
    this.browser = null;
    this.bonjour?.destroy();
    this.bonjour = null;
    this.scanning = false;
  }

  scan() {
    this.stop();
    const current = this.generation;
    this.hosts = [];
    this.nearbyHosts = [];
    this.pairedHosts = [];
    this.scanning = true;
    this.status = "Searching nearby and paired hosts…";
    this.publish();
    this.probePaired(current);
    try {
      this.bonjour = new Bonjour({}, (error) => this.unavailable(current, error));
      this.browser = this.bonjour.find({ type: "xedoc-gui", protocol: "tcp" });
      const changed = () => this.updateNearby(current);
      for (const event of ["up", "down", "txt-update", "srv-update"]) this.browser.on(event, changed);
    } catch (error) {
      this.unavailable(current, error);
      return;
    }
    this.timers.push(setTimeout(() => {
      if (this.generation !== current) return;
      this.scanning = false;
      this.status = this.hosts.length
        ? "Hosts available"
        : "No hosts found. Check that your firewall allows mDNS (UDP 5353) or enter a host URL.";
      this.publish();
    }, 5_000));
  }

  unavailable(current, error) {
    if (this.generation !== current) return;
    this.scanning = false;
    this.status = `Discovery unavailable: ${error?.message ?? error}. Check your firewall and network settings.`;
    this.browser?.stop();
    this.browser = null;
    this.publish();
  }

  updateNearby(current) {
    if (this.generation !== current || !this.browser) return;
    const discovered = [];
    for (const service of this.browser.services) {
      const txt = service.txt ?? {};
      const url = txt.version === "1" ? validatedHostURL(txt.origin) : null;
      const hostname = txt.hostname;
      if (!url || typeof hostname !== "string" || !hostname || hostname.length > 255 || discovered.length >= 64
          || discovered.some((host) => host.url === url.href)) continue;
      discovered.push({ hostname, url: url.href, route: "direct" });
    }
    this.nearbyHosts = discovered;
    this.updateHosts();
  }

  async probePaired(current) {
    while (this.generation === current) {
      const pending = pairing.pairedHosts();
      const worker = async () => {
        for (let host = pending.shift(); host; host = pending.shift()) {
          const available = await pairing.availableHost(host);
          if (this.generation !== current) return;
          this.pairedHosts = this.pairedHosts.filter((item) => item.url !== host.url);
          if (available) this.pairedHosts.push(available);
          this.updateHosts();
        }
      };
      await Promise.all(Array.from({ length: 4 }, worker));
      if (this.generation !== current) return;
      await new Promise((resolve) => this.timers.push(setTimeout(resolve, 15_000)));
    }
  }

  updateHosts() {
    const paired = this.pairedHosts.filter((host) => !this.nearbyHosts.some((nearby) => nearby.url === host.url));
    this.hosts = [...this.nearbyHosts, ...paired]
      .sort((left, right) => left.hostname.localeCompare(right.hostname, undefined, { numeric: true }));
    if (!this.scanning) this.status = this.hosts.length ? "Hosts available" : "No hosts found";
    this.publish();
  }
}

module.exports = { HostDiscovery };
