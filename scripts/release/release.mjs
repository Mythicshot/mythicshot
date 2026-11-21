#!/usr/bin/env node
// Cuts a signed Mythicshot release, or refuses. Node 24, standard library only.
//
//   node scripts/release/release.mjs [--config <file>] [--out <dir>] [--wix <wix.exe>]
//   node scripts/release/release.mjs --check-config [--config <file>]
//   node scripts/release/release.mjs --to-site <site checkout> [--from <release dir>] [--config <file>]
//
// See docs/release.md. Exit codes: 0 done, 2 bad command line, 3 signing configuration
// missing or incomplete, 4 build failed, 5 signing failed, 6 signature check failed,
// 7 versions differ, 8 the site refused, 9 uncommitted changes, 1 anything unexpected.
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { describeConfig } from './lib/config.mjs';
import { EXIT, Refusal } from './lib/errors.mjs';
import { cutRelease } from './lib/release.mjs';
import { releaseToSite } from './lib/site.mjs';
import { describeTools, resolveTools } from './lib/tools.mjs';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

const VALUE_FLAGS = new Set(['--config', '--out', '--wix', '--to-site', '--from']);
const SWITCHES = new Set(['--check-config', '--help']);

export function parseArgs(argv) {
  const opts = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (SWITCHES.has(arg)) {
      opts[arg.slice(2)] = true;
    } else if (VALUE_FLAGS.has(arg)) {
      const value = argv[++i];
      if (value === undefined || value.startsWith('--')) throw new Refusal(EXIT.USAGE, `${arg} needs a value.`);
      opts[arg.slice(2)] = value;
    } else {
      throw new Refusal(EXIT.USAGE, `Unknown argument "${arg}". Run with --help.`);
    }
  }
  if (opts['check-config'] && opts['to-site']) throw new Refusal(EXIT.USAGE, '--check-config and --to-site are separate commands; give one.');
  return opts;
}

const HELP = `Cut a signed Mythicshot release (see docs/release.md).
  node scripts/release/release.mjs [--config <file>] [--out <dir>] [--wix <wix.exe>]
  node scripts/release/release.mjs --check-config [--config <file>]
  node scripts/release/release.mjs --to-site <site checkout> [--from <release dir>] [--config <file>]`;

/** Runs the command; resolves to the exit code. Output goes through `out` and `err` (one line each). */
export async function main(argv, { env = process.env, out = console.log, err = console.error, runner, findSigntool, tools, repoRoot = REPO_ROOT } = {}) {
  try {
    const opts = parseArgs(argv);
    if (opts.help) {
      out(HELP);
      return EXIT.OK;
    }
    if (opts['check-config']) {
      const report = describeConfig({ path: opts.config, env, repoRoot, findSigntool });
      for (const line of report.lines) out(line);
      // The only place tool paths are printed.
      out('Tools (absolute paths; never from the checkout, the site or the current folder):');
      for (const line of describeTools(tools ?? resolveTools({ env, exclude: [repoRoot, process.cwd()], wix: opts.wix }))) out(`- ${line}`);
      return report.ok ? EXIT.OK : EXIT.CONFIG;
    }
    if (opts['to-site']) {
      await releaseToSite(
        { site: opts['to-site'], from: opts.from ?? join(repoRoot, 'dist', 'release'), configPath: opts.config, env, repoRoot },
        { runner, log: out, findSigntool, tools },
      );
      return EXIT.OK;
    }
    await cutRelease(
      { repoRoot, out: opts.out, configPath: opts.config, env, wix: opts.wix },
      { runner, log: out, findSigntool, tools },
    );
    return EXIT.OK;
  } catch (error) {
    if (error instanceof Refusal) {
      err(`Refused: ${error.message}`);
      return error.code;
    }
    // Unexpected errors are reported by kind only: their text could carry a configuration value.
    err(`Refused: an unexpected ${error?.name ?? 'error'} stopped the command (${error?.code ?? 'no code'}); nothing was released.`);
    return EXIT.UNEXPECTED;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  process.exitCode = await main(process.argv.slice(2));
}
