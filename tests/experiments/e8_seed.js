// E8 seeder — write a mappingToken into a chrome.storage.local LevelDB store,
// externally, the way the Synchrome app would before the browser first launches.
// Usage: node e8_seed.js <storeDir> <tokenJSON>
const { ClassicLevel } = require("/tmp/e8seed/node_modules/classic-level");
(async () => {
  const dir = process.argv[2];
  const tokenValue = process.argv[3]; // already a JSON string of the value
  const db = new ClassicLevel(dir);
  await db.open();
  // chrome.storage.local stores: key = setting key (plain), value = JSON of the value.
  await db.put("mappingToken", tokenValue);
  await db.close();
  console.log("seeded mappingToken into", dir);
})().catch(e => { console.error("seed error:", e); process.exit(1); });
