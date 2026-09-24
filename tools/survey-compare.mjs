// Compare two survey.txt files disc by disc: lit pixels, sectors read, MDEC
// macroblocks, textured primitives, unmapped reads, unknown CD commands.
//   node tools/survey-compare.mjs out/survey-OLD.txt out/survey/survey.txt
// Cells show old->new where they differ. A regression check, not a grade.
import fs from 'fs';
const parse = f => { const m = {}; let cur;
  for (const l of fs.readFileSync(f, 'utf8').split('\n')) {
    if (l.startsWith('== ')) { cur = l.slice(3); m[cur] = {}; continue; }
    if (!cur) continue;
    let x;
    if ((x = l.match(/(\d+) non-black/))) m[cur].px = +x[1];
    if ((x = l.match(/(\d+) sectors read/))) m[cur].sec = +x[1];
    if ((x = l.match(/mdec: (\d+)/))) m[cur].mdec = +x[1];
    if ((x = l.match(/gpu: (\d+) textured/))) m[cur].tex = +x[1];
    if ((x = l.match(/(\d+) unmapped reads/))) m[cur].unm = +x[1];
    if ((x = l.match(/\((\d+) unknown\)/))) m[cur].unk = +x[1];
  } return m; };
const [a, b] = [parse(process.argv[2]), parse(process.argv[3])];
const keys = ['px', 'sec', 'mdec', 'tex', 'unm', 'unk'];
console.log('game'.padEnd(42), keys.map(k => k.padStart(18)).join(''));
for (const g of Object.keys(b)) {
  const row = keys.map(k => { const x = a[g]?.[k], y = b[g][k]; return (x === y ? `${y}` : `${x}->${y}`).padStart(18); });
  console.log(g.slice(0, 41).padEnd(42), row.join(''));
}
