// Effect fidelity against After Effects (gaps.md G1): renders the scenes of
// examples/effects-fidelity.jsx in After Effects (through AEsync) and in EffectCraft, compares
// each pair of frames with `effectcraft-cli compare`, and writes the scores to
// docs/fidelity/effects.json. Frames stay in the gitignored plan/aftereffects/ref/effects/.
//
//   node examples/check-ae-effects.mjs --aesync /path/to/aesync/server/dist/cli.js \
//        [--ec-cli target/release/effectcraft-cli] [--only a,b] [--min-psnr 30] [--ec-only]
//
// --ec-only renders EffectCraft's frames alone (no After Effects), to check the harness.
// Run in a scratch After Effects project: the script adds and removes comps and solids.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const opt = name => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};
const aesync = opt('--aesync');
const ecOnly = args.includes('--ec-only');
const only = opt('--only') ? opt('--only').split(',').map(s => s.trim()).filter(Boolean) : null;
const minPsnr = Number(opt('--min-psnr') ?? 30);
if (!ecOnly && (!aesync || !fs.existsSync(aesync))) {
  throw new Error('usage: node examples/check-ae-effects.mjs --aesync <aesync/server/dist/cli.js> [--ec-cli PATH] [--only a,b] [--min-psnr DB] | --ec-only');
}
const ecCli = opt('--ec-cli') ? [path.resolve(opt('--ec-cli'))] : ['cargo', 'run', '--quiet', '--release', '-p', 'effectcraft-cli', '--'];

function run(cmd, cmdArgs, { json = true, allowFail = false } = {}) {
  const r = spawnSync(cmd, cmdArgs, { cwd: root, windowsHide: true, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  if (r.status !== 0 && !allowFail) throw new Error(`${cmd} ${cmdArgs.join(' ')} failed:\n${r.stderr || r.stdout}`);
  if (!json) return r;
  const text = (r.stdout || '').replace(/^﻿/, '').trim();
  const line = text.split('\n').reverse().find(l => l.trim().startsWith('{') || l.trim().startsWith('"{'));
  if (!line) throw new Error(`no JSON from ${cmd}:\n${text}\n${r.stderr}`);
  let v = JSON.parse(line.trim());
  if (typeof v === 'string') v = JSON.parse(v);
  return { value: v, status: r.status };
}

const refDir = path.join(root, 'plan', 'aftereffects', 'ref', 'effects');
const scene = fs.readFileSync(path.join(root, 'examples', 'effects-fidelity.jsx'), 'utf8');
function wrapper(app) {
  const out = path.join(refDir, app);
  fs.mkdirSync(out, { recursive: true });
  const file = path.join(os.tmpdir(), `ec-fidelity-${app}-${process.pid}.jsx`);
  fs.writeFileSync(file, `var EC_FIDELITY = ${JSON.stringify({ out, only, keep: false })};\n${scene}`);
  return file;
}

const ecRun = (() => {
  const [cmd, ...pre] = ecCli;
  return run(cmd, [...pre, 'script', wrapper('ec'), '--json']);
})().value;
const ec = typeof ecRun.result === 'string' ? JSON.parse(ecRun.result) : ecRun.result ?? ecRun;
if (!ec || !Array.isArray(ec.cases)) throw new Error('EffectCraft returned no cases: ' + JSON.stringify(ecRun).slice(0, 2000));
const failedEc = ec.cases.filter(c => !c.ok);
if (ecOnly) {
  console.log(JSON.stringify({ app: ec.app, cases: ec.cases.map(c => ({ name: c.name, ok: c.ok, error: c.error })) }, null, 2));
  if (failedEc.length) process.exitCode = 1;
  process.exit();
}

const ae = run(process.execPath, [aesync, 'run', '-f', wrapper('ae'), '--raw']).value;
const [cmd, ...pre] = ecCli;
const scores = ae.cases.map(ref => {
  const mine = ec.cases.find(c => c.name === ref.name);
  const s = { name: ref.name, effect: ref.effect, params: ref.params, target: ref.target };
  if (!ref.ok) return { ...s, pass: null, note: 'After Effects: ' + (ref.error ?? 'no frame') };
  if (!mine || !mine.ok) return { ...s, pass: false, note: 'EffectCraft: ' + (mine?.error ?? 'no frame') };
  const r = run(cmd, [...pre, 'compare', ref.file, mine.file, '--min-psnr', String(minPsnr), '--diff', path.join(refDir, `diff-${ref.name}.png`), '--json'], { allowFail: true }).value;
  return { ...s, psnrDb: r.psnr_db, ssim: r.ssim, meanAbs: r.mean_abs, maxAbs: r.max_abs, changedPct: r.changed_pct, pass: r.pass };
});
const measured = scores.filter(s => s.pass !== null);
const report = {
  generatedAt: new Date().toISOString(),
  aeVersion: ae.app,
  effectcraft: ec.app,
  scene: 'examples/effects-fidelity.jsx (320x180, Gradient Ramp + rotated Checkerboard, frame at 0.5 s)',
  minPsnrDb: minPsnr,
  passed: measured.filter(s => s.pass).length,
  measured: measured.length,
  cases: scores,
};
fs.mkdirSync(path.join(root, 'docs', 'fidelity'), { recursive: true });
fs.writeFileSync(path.join(root, 'docs', 'fidelity', 'effects.json'), JSON.stringify(report, null, 2) + '\n');
console.log(`${report.passed} of ${report.measured} effect cases within ${minPsnr} dB of After Effects ${ae.app}`);
for (const s of scores) console.log(`${s.pass === true ? 'pass' : s.pass === false ? 'FAIL' : 'skip'}  ${s.name}  ${s.psnrDb ?? ''} ${s.note ?? ''}`);
if (measured.some(s => !s.pass)) process.exitCode = 1;
