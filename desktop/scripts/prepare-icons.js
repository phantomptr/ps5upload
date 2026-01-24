const fs = require('fs');
const path = require('path');
const Jimp = require('jimp');

const root = path.resolve(__dirname, '..');
const sourceCandidates = [
  path.join(root, 'public', 'logo.png'),
  path.resolve(root, '..', 'logo.png'),
];
const source = sourceCandidates.find((p) => fs.existsSync(p));

if (!source) {
  console.error('Icon source not found. Expected logo.png in desktop/public or repo root.');
  process.exit(1);
}

const outputDir = path.join(root, 'build');
const outputPath = path.join(outputDir, 'icon.png');
const size = 1024;

async function run() {
  await fs.promises.mkdir(outputDir, { recursive: true });
  const image = await Jimp.read(source);
  const width = image.bitmap.width;
  const height = image.bitmap.height;
  const max = Math.max(width, height);
  const canvas = new Jimp(max, max, 0x00000000);
  const x = Math.floor((max - width) / 2);
  const y = Math.floor((max - height) / 2);
  canvas.composite(image, x, y);
  canvas.resize(size, size, Jimp.RESIZE_BICUBIC);
  await canvas.writeAsync(outputPath);
  console.log(`Icon generated: ${outputPath}`);
}

run().catch((err) => {
  console.error(`Failed to generate icon: ${err.message}`);
  process.exit(1);
});
