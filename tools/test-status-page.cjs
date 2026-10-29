// Host regressions for the offline status page. Run: node --test tools/test-status-page.cjs
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function page() {
  const elements = new Map();
  class Element {
    constructor(fragment = false) { this.children = []; this.fragment = fragment; this.textContent = ''; this.classes = new Set(); this.classList = {add: value => this.classes.add(value)}; this.replacements = 0; }
    replaceChildren() { this.children = []; this.replacements++; }
    appendChild(child) { this.children.push(...(child.fragment ? child.children : [child])); }
  }
  const document = {
    getElementById(id) { if (!elements.has(id)) elements.set(id, new Element()); return elements.get(id); },
    createElement: () => new Element(), createDocumentFragment: () => new Element(true),
  };
  const context = vm.createContext({document, setTimeout() {}, fetch() { throw Error('unexpected fetch'); }});
  const html = fs.readFileSync(require('node:path').join(__dirname, '../app-esp32/static/status.html'), 'utf8');
  const script = html.match(/<script>([\s\S]*?)<\/script>/)[1].replace('pollRattle(); pollDevice();', '');
  vm.runInContext(script, context);
  return {context, element: id => document.getElementById(id), evaluate: code => vm.runInContext(code, context)};
}

function run(step, playing = true) {
  return {playing, current: playing ? {step, steps:4096, label:'40 Hz @ -35 dB', cycle:0, cycles:64, phase:'tone', remainingSec:1} : undefined,
    last:{startedAtUs:1, state:playing ? 'playing' : 'completed', schedule:{frequenciesHz:Array.from({length:64}, (_, i) => 20+i), volumesDb:[-35], cycles:64, leadInSamples:12000, toneSamples:24000, gapSamples:0, sampleRate:48000}}};
}

test('fast rattle poll refreshes shared busy state at completion', async () => {
  const p = page(); p.context.status = run(30); p.evaluate('playBusy = true; render(status)');
  const paths = [];
  p.context.fetch = async path => {
    paths.push(path);
    return {ok:true, json:async () => path.endsWith('/play') ? {playing:false} : run(30, false)};
  };
  await p.evaluate('pollRattle()');
  assert.ok(paths.includes('/devices/0/play'));
  assert.equal(p.element('playback').textContent, 'Idle');
  p.context.status = run(30); p.evaluate('playBusy = true; render(status)');
  assert.equal(p.element('playback').textContent, 'Rattle running');
  p.context.status = {playing:false}; p.evaluate('render(status)');
  assert.equal(p.element('playback').textContent, '/play running');
});

test('maximum schedule renders at most 31 rows and skips unchanged polls', () => {
  const p = page();
  for (const step of [0, 2000, 4095]) {
    p.context.status = run(step); p.evaluate('render(status)');
    const list = p.element('schedule');
    assert.ok(list.children.length <= 31);
    assert.equal(p.element('schedule-position').textContent, `Step ${step+1} of 4096`);
    assert.equal(list.start, Math.max(0, step-10)+1);
    assert.equal(list.children.filter(child => child.classes.has('current')).length, 1);
    const replacements = list.replacements;
    p.evaluate('render(status)');
    assert.equal(list.replacements, replacements);
  }
});

test('known sources use readable names and unknown names are preserved', () => {
  const p = page();
  for (const [input, expected] of Object.entries({Usb:'USB', Toslink:'TOSLINK', Spdif:'S/PDIF', Analog:'Analog', Bluetooth:'Bluetooth'})) {
    p.context.source = input;
    assert.equal(p.evaluate('displaySource(source)'), expected);
  }
  for (const source of ['toString', '__proto__']) {
    p.context.source = source;
    assert.equal(p.evaluate('displaySource(source)'), source);
  }
});

test('fractional volume is rejected before submitting or confirming playback', async () => {
  const p = page(); p.evaluate('connected = true');
  p.element('volumes').value = '-35.2'; p.element('tones').value = '40';
  await p.element('form').onsubmit({preventDefault() {}});
  assert.equal(p.element('message').textContent, 'Volumes must use multiples of 0.5 dB.');
});
