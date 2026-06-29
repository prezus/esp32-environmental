//! HTTP server: a small dashboard plus a JSON API and raw-CSV download for export.

use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write as EspWrite;
use std::io::{BufRead, BufReader};

use crate::storage;
use crate::{Calibration, SdGuard, SharedLatest};

const JSON: &[(&str, &str)] = &[("Content-Type", "application/json")];

/// Chart.js (gzip-compressed), embedded into the firmware and served from the device
/// (no CDN). Serving it pre-gzipped keeps the response ~68 KB instead of ~200 KB.
const CHART_JS_GZ: &[u8] = include_bytes!("../assets/chart.umd.min.js.gz");

/// Build and start the HTTP server. The returned handle must be kept alive for the
/// server to keep running.
pub fn start(
    latest: SharedLatest,
    sd: SdGuard,
    calibration: Calibration,
) -> anyhow::Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&Configuration {
        stack_size: 10_240,
        // The dashboard fires several requests at once (page, /chart.js, the APIs).
        // Allow more concurrent sockets so they don't evict each other (ECONNRESET).
        max_open_sockets: 7,
        ..Default::default()
    })?;

    // Dashboard.
    server.fn_handler::<anyhow::Error, _>("/", Method::Get, |req| {
        req.into_ok_response()?
            .write_all(DASHBOARD_HTML.as_bytes())?;
        Ok(())
    })?;

    // Chart.js, served locally (gzipped) so the dashboard never needs the internet.
    server.fn_handler::<anyhow::Error, _>("/chart.js", Method::Get, |req| {
        let headers = [
            ("Content-Type", "application/javascript"),
            ("Content-Encoding", "gzip"),
            ("Cache-Control", "max-age=86400"),
        ];
        req.into_response(200, Some("OK"), &headers)?
            .write_all(CHART_JS_GZ)?;
        Ok(())
    })?;

    // Latest reading as JSON.
    let latest_api = latest.clone();
    server.fn_handler::<anyhow::Error, _>("/api/latest", Method::Get, move |req| {
        let body = match &*latest_api.lock().unwrap() {
            Some(r) => format!(
                "{{\"iso8601\":\"{}\",\"temp_c\":{},\"temp_f\":{},\"humidity_pct\":{}}}",
                r.iso8601,
                jnum(r.temperature_c),
                jnum(r.temperature_f),
                jnum(r.humidity_pct)
            ),
            None => "{}".to_string(),
        };
        req.into_response(200, Some("OK"), JSON)?
            .write_all(body.as_bytes())?;
        Ok(())
    })?;

    // List of available log dates.
    let sd_files = sd.clone();
    server.fn_handler::<anyhow::Error, _>("/api/files", Method::Get, move |req| {
        let dates = {
            let _g = sd_files.lock().unwrap();
            storage::list_dates().unwrap_or_default()
        };
        let items: Vec<String> = dates.iter().map(|d| format!("\"{d}\"")).collect();
        let body = format!("[{}]", items.join(","));
        req.into_response(200, Some("OK"), JSON)?
            .write_all(body.as_bytes())?;
        Ok(())
    })?;

    // One day's data as JSON for the chart.
    let sd_data = sd.clone();
    server.fn_handler::<anyhow::Error, _>("/api/data", Method::Get, move |req| {
        let date = query_param(req.uri(), "date").filter(|d| storage::is_valid_date(d));
        let Some(date) = date else {
            req.into_response(400, Some("Bad Request"), &[])?
                .write_all(b"missing or invalid 'date'")?;
            return Ok(());
        };
        let result = {
            let _g = sd_data.lock().unwrap();
            stream_data_json(&date, req)
        };
        result
    })?;

    // Raw CSV download (export).
    let sd_dl = sd.clone();
    server.fn_handler::<anyhow::Error, _>("/download", Method::Get, move |req| {
        let date = query_param(req.uri(), "date").filter(|d| storage::is_valid_date(d));
        let Some(date) = date else {
            req.into_response(400, Some("Bad Request"), &[])?
                .write_all(b"missing or invalid 'date'")?;
            return Ok(());
        };
        let result = {
            let _g = sd_dl.lock().unwrap();
            stream_csv_download(&date, req)
        };
        result
    })?;

    // Read the current temperature-calibration offset.
    let cal_get = calibration.clone();
    server.fn_handler::<anyhow::Error, _>("/api/config", Method::Get, move |req| {
        let off = *cal_get.lock().unwrap();
        let body = format!("{{\"temp_offset_f\":{}}}", jnum(off));
        req.into_response(200, Some("OK"), JSON)?
            .write_all(body.as_bytes())?;
        Ok(())
    })?;

    // Set the temperature-calibration offset (°F), e.g. POST /api/config?temp_offset_f=-5
    // The sampling loop applies it to new readings and persists it to NVS.
    let cal_set = calibration.clone();
    server.fn_handler::<anyhow::Error, _>("/api/config", Method::Post, move |req| {
        match query_param(req.uri(), "temp_offset_f").and_then(|s| s.parse::<f32>().ok()) {
            Some(v) if v.is_finite() => {
                let v = v.clamp(-20.0, 20.0);
                *cal_set.lock().unwrap() = v;
                log::info!("temperature offset set via web: {v:+.1} °F");
                let body = format!("{{\"temp_offset_f\":{}}}", jnum(v));
                req.into_response(200, Some("OK"), JSON)?
                    .write_all(body.as_bytes())?;
            }
            _ => {
                req.into_response(400, Some("Bad Request"), &[])?
                    .write_all(b"missing or invalid 'temp_offset_f'")?;
            }
        }
        Ok(())
    })?;

    Ok(server)
}

/// Shared "latest reading" used by `/api/latest`.
#[derive(Clone, Debug)]
pub struct Latest {
    pub iso8601: String,
    pub temperature_c: f32,
    pub temperature_f: f32,
    pub humidity_pct: f32,
}

/// Format a float as a JSON number, or `null` if it isn't finite. JSON has no `NaN`
/// literal, and browsers' `JSON.parse` rejects it — emitting `NaN` would break the
/// whole response (and thus the chart).
fn jnum(v: f32) -> String {
    if v.is_finite() {
        format!("{v:.2}")
    } else {
        "null".to_string()
    }
}

fn stream_data_json(
    date: &str,
    req: esp_idf_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection>,
) -> anyhow::Result<()> {
    let file = match storage::open_csv(date) {
        Ok(file) => file,
        Err(_) => {
            req.into_response(404, Some("Not Found"), &[])?
                .write_all(b"no such log file")?;
            return Ok(());
        }
    };

    let mut resp = req.into_response(200, Some("OK"), JSON)?;
    resp.write_all(b"{\"points\":[")?;
    let mut first = true;
    for line in BufReader::new(file).lines().skip(1) {
        let line = line?;
        let mut cols = line.split(',');
        let iso = cols.next().unwrap_or_default();
        let _unix = cols.next();
        let tc = cols.next().and_then(|s| s.parse().ok()).unwrap_or(f32::NAN);
        let tf = cols.next().and_then(|s| s.parse().ok()).unwrap_or(f32::NAN);
        let rh = cols.next().and_then(|s| s.parse().ok()).unwrap_or(f32::NAN);
        if iso.is_empty() {
            continue;
        }
        if !first {
            resp.write_all(b",")?;
        }
        first = false;
        let point = format!(
            "{{\"t\":\"{}\",\"tc\":{},\"tf\":{},\"rh\":{}}}",
            iso,
            jnum(tc),
            jnum(tf),
            jnum(rh)
        );
        resp.write_all(point.as_bytes())?;
    }
    resp.write_all(b"]}")?;
    Ok(())
}

fn stream_csv_download(
    date: &str,
    req: esp_idf_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection>,
) -> anyhow::Result<()> {
    let mut file = match storage::open_csv(date) {
        Ok(file) => file,
        Err(_) => {
            req.into_response(404, Some("Not Found"), &[])?
                .write_all(b"no such log file")?;
            return Ok(());
        }
    };

    let disp = format!("attachment; filename=\"{date}.csv\"");
    let headers = [
        ("Content-Type", "text/csv"),
        ("Content-Disposition", disp.as_str()),
    ];
    let mut resp = req.into_response(200, Some("OK"), &headers)?;
    let mut buf = [0u8; 1024];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        resp.write_all(&buf[..n])?;
    }
    Ok(())
}

/// Extract a query parameter value from a URI like "/api/data?date=2026-06-26".
fn query_param(uri: &str, key: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ESP32 Environmental Logger</title>
<script src="/chart.js"></script>
<style>
  body { font-family: system-ui, sans-serif; margin: 1.5rem; color: #222; }
  h1 { font-size: 1.25rem; } h2 { font-size: 1rem; margin: 1.25rem 0 .25rem; color: #444; }
  .cards { display: flex; gap: 1rem; flex-wrap: wrap; margin: 1rem 0; }
  .card { border: 1px solid #ddd; border-radius: 8px; padding: 1rem 1.5rem; min-width: 8rem; }
  .card .val { font-size: 2rem; font-weight: 600; }
  .card .lbl { color: #666; font-size: .85rem; }
  .controls { margin: 1rem 0; }
  a.dl { margin-left: .75rem; }
  .chart-wrap { max-width: 760px; }
  canvas { max-width: 100%; }
</style>
</head>
<body>
<h1>ESP32 Environmental Logger</h1>
<div class="cards">
  <div class="card"><div class="val" id="tempc">--</div><div class="lbl">Temperature (&deg;C)</div></div>
  <div class="card"><div class="val" id="tempf">--</div><div class="lbl">Temperature (&deg;F)</div></div>
  <div class="card"><div class="val" id="rh">--</div><div class="lbl">Humidity (%)</div></div>
  <div class="card"><div class="val" id="time" style="font-size:1rem">--</div><div class="lbl">Last reading (local)</div></div>
</div>
<div class="controls">
  <label>Day: <select id="day"></select></label>
  <a class="dl" id="dl" href="#">Download CSV</a>
</div>
<div class="controls">
  <label>Temp calibration offset (&deg;F):
    <input id="off" type="number" step="0.1" style="width:6rem">
  </label>
  <button id="saveCal">Save</button>
  <span id="calMsg" style="margin-left:.5rem;color:#666"></span>
</div>
<h2>Temperature</h2>
<div class="chart-wrap"><canvas id="tempChart" height="120"></canvas></div>
<h2>Humidity</h2>
<div class="chart-wrap"><canvas id="humChart" height="120"></canvas></div>
<script>
let tempChart, humChart;
function upsert(chart, canvasId, data, opts) {
  if (typeof Chart === 'undefined') return chart;
  if (chart) { chart.data = data; chart.update(); return chart; }
  return new Chart(document.getElementById(canvasId), { type: 'line', data, options: opts });
}
async function loadLatest() {
  try {
    const r = await (await fetch('/api/latest')).json();
    if (r.iso8601) {
      document.getElementById('tempc').textContent = r.temp_c.toFixed(1);
      document.getElementById('tempf').textContent = r.temp_f.toFixed(1);
      document.getElementById('rh').textContent = r.humidity_pct.toFixed(1);
      document.getElementById('time').textContent = r.iso8601;
    }
  } catch (e) {}
}
async function loadDays() {
  try {
    const days = await (await fetch('/api/files')).json();
    const sel = document.getElementById('day');
    sel.innerHTML = '';
    days.forEach(d => { const o = document.createElement('option'); o.value = d; o.textContent = d; sel.appendChild(o); });
    if (days.length) loadDay(days[0]);
  } catch (e) {}
}
async function loadDay(date) {
  document.getElementById('dl').href = '/download?date=' + date;
  try {
    const data = await (await fetch('/api/data?date=' + date)).json();
    const labels = data.points.map(p => p.t.substring(11, 19));
    const tc = data.points.map(p => p.tc);
    const tf = data.points.map(p => p.tf);
    const rh = data.points.map(p => p.rh);
    const base = { interaction: { mode: 'index', intersect: false }, animation: false, plugins: { legend: { display: true } } };
    tempChart = upsert(tempChart, 'tempChart', {
      labels, datasets: [
        { label: '°C', data: tc, borderColor: '#e4572e', backgroundColor: '#e4572e', yAxisID: 'yc', tension: .25, pointRadius: 2 },
        { label: '°F', data: tf, borderColor: '#f3a712', backgroundColor: '#f3a712', yAxisID: 'yf', tension: .25, pointRadius: 2 },
      ]
    }, Object.assign({}, base, { scales: {
        yc: { position: 'left', title: { display: true, text: '°C' } },
        yf: { position: 'right', title: { display: true, text: '°F' }, grid: { drawOnChartArea: false } },
    }}));
    humChart = upsert(humChart, 'humChart', {
      labels, datasets: [
        { label: '% RH', data: rh, borderColor: '#3185fc', backgroundColor: 'rgba(49,133,252,.12)', fill: true, tension: .25, pointRadius: 2 },
      ]
    }, Object.assign({}, base, { scales: { y: { title: { display: true, text: '% RH' }, suggestedMin: 0, suggestedMax: 100 } } }));
  } catch (e) {}
}
async function loadConfig() {
  try {
    const c = await (await fetch('/api/config')).json();
    document.getElementById('off').value = (c.temp_offset_f ?? 0).toFixed(1);
  } catch (e) {}
}
document.getElementById('day').addEventListener('change', e => loadDay(e.target.value));
document.getElementById('saveCal').addEventListener('click', async () => {
  const v = parseFloat(document.getElementById('off').value || '0');
  const msg = document.getElementById('calMsg');
  try {
    const c = await (await fetch('/api/config?temp_offset_f=' + encodeURIComponent(v), { method: 'POST' })).json();
    document.getElementById('off').value = (c.temp_offset_f ?? 0).toFixed(1);
    msg.textContent = 'Saved ' + (c.temp_offset_f >= 0 ? '+' : '') + c.temp_offset_f.toFixed(1) + '°F. Applies to new readings.';
  } catch (e) { msg.textContent = 'Save failed'; }
});
loadLatest(); loadDays(); loadConfig();
setInterval(loadLatest, 5000);
setInterval(() => { const s = document.getElementById('day'); if (s.value) loadDay(s.value); }, 30000);
</script>
</body>
</html>"##;
