"use strict";
const $ = (id) => document.getElementById(id);
const post = (m) => window.webkit.messageHandlers.nm.postMessage(m);
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const S = { mac: null, pkg: "", previewed: false, installed: false };

function row(k, v) { return `<tr><th>${esc(k)}</th><td>${v}</td></tr>`; }
function step(n, state) { const li = $("s" + n); li.classList.toggle("done", state === "done"); li.classList.toggle("now", state === "now"); }

function render(m) {
  S.mac = m;
  $("ver").textContent = m.version;
  const card = m.gpus.find((g) => g.supported) || m.gpus.find((g) => g.vendor === "10DE");
  const on = m.kexts >= 4 && m.metal.some((n) => /NVIDIA|GeForce|RTX/i.test(n));
  $("mac").innerHTML =
    row("macOS", esc(m.macos) + ([15, 26].includes(m.major) ? "" : ' <span class="bad">(the driver is for macOS 15 and 26)</span>')) +
    row("Processor", m.arch === "x86_64" && !m.translated ? "x86_64" : '<span class="bad">Apple silicon - this driver is for Intel Macs and PCs</span>') +
    row("Graphics", m.gpus.length ? m.gpus.map((g) => esc(g.name) + (g.supported ? (g.tested ? ' <span class="good">listed device, model tested</span>' : ' <span class="warn">listed by NVIDIA, model not tested yet</span>') : g.vendor === "10DE" ? ' <span class="bad">not supported (needs RTX 20 series or newer)</span>' : "")).join("<br>") : "none found") +
    row("OpenCore", m.opencore ? esc(m.opencore) : '<span class="warn">not detected</span>') +
    row("NullMoth driver", on ? '<span class="good">running</span> (' + esc(m.metal.join(", ")) + ")" : m.files ? '<span class="warn">installed, not running yet</span>' : "not installed");

  let why = "";
  if (m.arch !== "x86_64" || m.translated) why = "This Mac has Apple silicon. Its own graphics are already Apple's.";
  else if (![15, 26].includes(m.major)) why = `The driver is built for macOS 15 Sequoia and macOS 26 Tahoe (beta); this Mac runs ${esc(m.macos)}.`;
  else if (!card || !card.supported) why = "No supported NVIDIA card was found. The driver needs a GeForce RTX 20 series or newer.";
  else if (!m.opencore) why = "This Mac does not report OpenCore. The driver's settings live in OpenCore's config, so 1401 will not install without it.";

  // Keep the checked package available when the driver is already running.
  const pk = m.packages.find((p) => p.ok === "yes"); if (pk) S.pkg = pk.path;
  if (on) {
    $("verdict").innerHTML = `<p class="good">NullMoth components are loaded and an NVIDIA Metal device is visible.</p><p>Use Check for a newer driver to update the driver. To remove the driver, use Remove the driver below.</p>`;
    $("steps").hidden = true; return;
  }
  if (why) { $("verdict").innerHTML = `<p class="bad">Can't install here.</p><p>${why}</p>`; $("steps").hidden = true; return; }
  if (m.files && !S.installed) {
    $("verdict").innerHTML = '<p class="warn">The driver is installed but not running.</p><p>Restart. If macOS blocked a system extension, allow it in Privacy &amp; Security, then restart once more.</p>';
  } else {
    $("verdict").innerHTML = `<p>Your <b>${esc(card.name)}</b> has a driver configuration available. This machine still needs boot, display and application checks. Four steps, in order.</p>`;
  }
  $("steps").hidden = false;
  const good = m.packages.find((p) => p.ok === "yes");
  if (good) { S.pkg = good.path; $("s1t").innerHTML = `Found on ${esc(good.where)}, checksum OK.`; $("dl").hidden = true; step(1, "done"); }
  else { $("s1t").textContent = m.packages.length ? "A copy was found but its checksum is wrong. Download a fresh one." : "Not on this Mac yet."; $("dl").hidden = false; step(1, "now"); }
  $("dry").disabled = !S.pkg;
  $("go").disabled = !S.pkg || !S.previewed;
  if (S.pkg && !S.previewed) step(2, "now");
}

function logLine(l) {
  const kind = l.split(" ")[0], rest = esc(l.slice(kind.length + 1));
  const cls = { STEP: "s", CHANGE: "c", OK: "k", STOP: "x", RESULT: "s" }[kind];
  const txt = kind === "STEP" ? "== " + rest : kind === "CHANGE" ? "will change: " + rest : kind === "OK" ? "ok  " + rest : kind === "STOP" ? "STOPPED: " + rest : kind === "NOTE" ? "    " + rest : kind === "RESULT" ? (rest === "ok" ? "done." : "stopped - nothing after the red line was changed.") : esc(l);
  $("log").insertAdjacentHTML("beforeend", (cls ? `<span class="${cls}">${txt}</span>` : txt) + "\n");
  $("log").scrollTop = $("log").scrollHeight;
  const m = l.match(/^NOTE candidate (\S+)/);
  if (m) { $("efirow").hidden = false; if (![...$("efi").options].some((o) => o.value === m[1])) $("efi").insertAdjacentHTML("beforeend", `<option value="${esc(m[1])}">${esc(m[1])}</option>`); }
}

const NM = {
  on({ event, data }) {
    if (event === "scan") render(data);
    if (event === "lines") data.forEach(logLine);
    if (event === "dl") {
      if (data.state === "progress" && data.total > 0) $("dlp").textContent = Math.floor((100 * data.done) / data.total) + "%";
      if (data.state === "start") ["dl", "updchk", "upddrv"].forEach((b) => ($(b).disabled = true));
      if (data.state === "error" || data.state === "done") ["dl", "updchk", "upddrv"].forEach((b) => ($(b).disabled = false));
      if (data.state === "error") $("dlp").innerHTML = `<span class="bad">${esc(data.why)}</span>`;
      if (data.state === "done") { $("dlp").textContent = "done, checksum OK"; post({ act: "scan" }); }
    }
    if (event === "upd") {
      if (data.state === "error") { $("updr").innerHTML = `<span class="bad">${esc(data.why)}</span>`; $("upddrv").hidden = true; }
      if (data.state === "checked") {
        const have = data.installed ? `installed ${esc(data.installed)}, ` : "";
        $("updr").innerHTML = data.newer ? `${have}newest ${esc(data.latest)} - ready to update.` : `${have}you have the newest driver (${esc(data.latest)}).`;
        $("upddrv").hidden = !data.newer;
        $("upddrv").disabled = !data.newer;
      }
    }
    if (event === "run") {
      const busy = data.state === "start";
      ["dry", "go", "rm", "dl", "updchk", "upddrv"].forEach((b) => ($(b).disabled = busy));
      if (busy) { $("logwrap").hidden = false; $("log").textContent = ""; return; }
      if (data.why) { $("logwrap").hidden = false; logLine(data.why); }
      if (data.state === "cancelled") { logLine("NOTE cancelled - nothing was changed"); post({ act: "scan" }); return; }
      if (data.mode === "dry" && data.ok) { S.previewed = true; step(2, "done"); step(3, "now"); }
      if (data.mode === "install" && data.ok) { S.installed = true; step(3, "done"); step(4, "now"); $("rs").disabled = false; }
      post({ act: "scan" });
    }
  },
};
window.NM = NM;

const efi = () => ($("efirow").hidden ? "auto" : $("efi").value);
$("efi").onchange = () => {
  S.previewed = false;
  $("go").disabled = true;
  step(2, "now"); step(3, "waiting");
};
$("dl").onclick = () => post({ act: "download" });
$("updchk").onclick = () => { $("updr").textContent = "Checking..."; post({ act: "checkUpdate" }); };
$("upddrv").onclick = () => { $("upddrv").disabled = true; $("updr").textContent = "Downloading the newest driver..."; post({ act: "updateDriver", efi: efi() }); };
$("ocg").onclick = (e) => { e.preventDefault(); post({ act: "open", url: "https://dortania.github.io/OpenCore-Install-Guide/" }); };
$("dry").onclick = () => post({ act: "run", mode: "dry", pkg: S.pkg, efi: efi() });
$("go").onclick = () => post({ act: "run", mode: "install", pkg: S.pkg, efi: efi() });
$("rm").onclick = () => { if (confirm("Remove the NullMoth driver and put your OpenCore config back the way it was?")) post({ act: "run", mode: "remove", pkg: "", efi: efi() }); };
$("rs").onclick = () => post({ act: "restart" });
$("vbon").onclick = () => post({ act: "verbose", on: true, efi: efi() });
$("vboff").onclick = () => post({ act: "verbose", on: false, efi: efi() });
$("priv").onclick = (e) => { e.preventDefault(); post({ act: "privacy" }); };
$("logs").onclick = (e) => { e.preventDefault(); post({ act: "logs" }); };
$("src").onclick = (e) => { e.preventDefault(); post({ act: "open", url: "https://github.com/nullmoth/nvidia-macos-driver" }); };
post({ act: "scan" });

document.querySelectorAll(".tab").forEach((b) => (b.onclick = () => {
  document.querySelectorAll(".tab").forEach((x) => x.classList.toggle("on", x === b));
  ["drv", "usb", "crash"].forEach((t) => ($("t-" + t).hidden = t !== b.dataset.t));
  if (b.dataset.t !== "usb") post({ act: "usbStop" });
}));
const CONN = [[0, "USB 2 Type-A"], [3, "USB 3 Type-A"], [9, "USB-C (flips)"], [10, "USB-C (one way)"], [255, "Inside the case"]];
function usbConnectorDefault(port) {
  if (port.connectorKnown === true && CONN.some(([value]) => value === port.connector)) return port.connector;
  return port.usb3 ? 3 : 0;
}
const U = { ctrls: [], seen: {}, pick: {}, off: new Set() };
function usbRender() {
  let h = "";
  for (const c of U.ctrls) {
    const name = c.key || c.controller, seen = new Set(U.seen[name] || []);
    const n = Object.keys(U.pick[name] || {}).length;
    h += `<table class="pc ports" border="1" cellspacing="2" cellpadding="3"><caption>Controller ${esc(c.controller)} (${esc(c.vendor)}:${esc(c.device)}) &mdash; <span class="${n > 15 ? "bad" : ""}">${n} of 15 picked</span></caption><tr><th>Use</th><th>Port</th><th>Seen</th><th>Plug</th><th>Device now</th></tr>`;
    for (const p of c.ports) {
      const on = seen.has(p.name), pk = (U.pick[name] || {})[p.name];
      const def = usbConnectorDefault(p);
      h += `<tr><td><input type="checkbox" data-c="${esc(name)}" data-p="${esc(p.name)}" ${pk !== undefined ? "checked" : ""}></td><td>${esc(p.name)}${p.usb3 ? " (USB 3)" : ""}</td><td class="${on ? "on" : "off"}">${on ? "yes" : "no"}</td>` +
        `<td><select data-c="${esc(name)}" data-p="${esc(p.name)}">${CONN.map(([v, t]) => `<option value="${v}" ${(pk ?? def) === v ? "selected" : ""}>${t}</option>`).join("")}</select></td><td>${esc((p.devices || []).join(", "))}</td></tr>`;
    }
    h += "</table>";
  }
  $("ports").innerHTML = h;
  $("ports").querySelectorAll("input").forEach((i) => (i.onchange = () => {
    const c = i.dataset.c, p = i.dataset.p; U.pick[c] = U.pick[c] || {};
    if (i.checked) { U.off.delete(c + "/" + p); U.pick[c][p] = +i.closest("tr").querySelector("select").value; } else { U.off.add(c + "/" + p); delete U.pick[c][p]; }
    usbRender();
  }));
  $("ports").querySelectorAll("select").forEach((s) => (s.onchange = () => { const c = s.dataset.c; if (U.pick[c] && s.dataset.p in U.pick[c]) U.pick[c][s.dataset.p] = +s.value; }));
}
$("uw").onclick = () => { U.pick = {}; U.off = new Set(); post({ act: "usbStart" }); $("uw").textContent = "Watching..."; $("uw").disabled = true; };
$("uwr").onclick = () => { $("uerr").textContent = ""; post({ act: "usbStop" }); post({ act: "usbWrite", sel: U.pick, efi: efi() }); };
$("mk").onclick = () => post({ act: "crashReport" });
$("cf").onclick = () => post({ act: "open", url: "https://buymeacoffee.com/nullmoth" });
$("sl").onclick = () => { $("sl").disabled = true; $("slr").textContent = "Collecting and sending..."; post({ act: "sendLogs" }); };
$("fl").onclick = () => { $("sl").disabled = true; $("fl").disabled = true; $("slr").textContent = "Recording the display for 20 seconds, then sending..."; post({ act: "sendLogs", sample: "flicker" }); };
$("od").onclick = () => post({ act: "optionalDiagnostics" });
$("dr").onclick = () => post({ act: "importDiagnosticReceipt" });
$("up").onclick = () => post({ act: "open", url: "https://nullmothsystems.com/#send" });
const prevOn = NM.on;
NM.on = (m) => {
  if (m.event === "diagnosticsStatus") { $("odr").textContent = m.data.text; return; }
  if (m.event === "diagnosticReceipt") { $("drr").textContent = m.data.text; return; }
  if (m.event === "usb") {
    U.ctrls = m.data.controllers; U.seen = m.data.seen;
    for (const c of U.ctrls) {   // a port that has been seen is ticked automatically, with its default plug type
      const key = c.key || c.controller;
      U.pick[key] = U.pick[key] || {};
      for (const p of c.ports) if ((U.seen[key] || []).includes(p.name) && !(p.name in U.pick[key]) && !U.off.has(key + "/" + p.name)) U.pick[key][p.name] = usbConnectorDefault(p);
    }
    usbRender(); return;
  }
  if (m.event === "usbErr") { $("uerr").textContent = m.data; return; }
  if (m.event === "logsStatus") { $("slr").textContent = m.data.text; return; }
  if (m.event === "logsDone") {
    const d = m.data;
    $("sl").disabled = false; $("fl").disabled = false;
    $("slr").innerHTML = d.ok ? `<span class="good">Sent. Report ID ${esc(d.ids.join(", "))}</span> - quote it when you ask for help.` + (d.errors.length ? `<br><span class="warn">Not sent: ${esc(d.errors.join("; "))}</span>` : "")
                              : `<span class="warn">${esc(d.why || ("Nothing was sent: " + (d.errors || []).join("; ")))}</span>`;
    if (d.localPath) $("slr").innerHTML += "<br>Report files are saved locally. Open Logs to find them or retry Send logs.";
    return;
  }
  if (m.event === "crashDone") { $("cr").innerHTML = m.data.ok ? `<span class="good">Saved to your Desktop:</span> ${esc(m.data.path.split("/").pop())}` : `<span class="warn">${esc(m.data.why)}</span>`; return; }
  if (m.event === "scan" && m.data.crashes) $("cr").textContent = m.data.crashes.length ? `${m.data.crashes.length} crash file(s) on this Mac name the driver.` : "No crash that names the driver is on this Mac.";
  if (m.event === "scan" && m.data.safemode && m.data.record) {
    setTimeout(() => $("verdict").insertAdjacentHTML("afterbegin", '<p class="warn">This Mac started with the NVIDIA driver switched off. To take it out for good, click Remove the driver below.</p>'), 0);
  }
  prevOn(m);
};
window.NM = NM;
