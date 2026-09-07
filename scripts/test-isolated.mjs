import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { lstat, mkdir, mkdtemp, readFile, readdir, readlink, rm } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// This is test configuration isolation, not a sandbox for arbitrary commands.
export function protectedConfigPaths(env = process.env, platform = process.platform) {
  const userHome = homedir();
  const paths = [];
  // etcetera::choose_app_strategy uses XDG on Unix (including macOS).
  if (platform !== 'win32') {
    paths.push(join(env.XDG_CONFIG_HOME || join(userHome, '.config'), 'esi-studio'));
  } else {
    paths.push(join(env.APPDATA || join(userHome, 'AppData/Roaming'), 'ESI/esi-studio/config'));
  }
  if (env.GOOSE_PATH_ROOT && isAbsolute(env.GOOSE_PATH_ROOT)) {
    paths.push(join(env.GOOSE_PATH_ROOT, 'config'));
  }
  return [...new Set(paths.map((path) => resolve(path)))];
}

export async function configDigest(paths) {
  const hash = createHash('sha256');
  async function visit(path) {
    hash.update(JSON.stringify(path));
    let stat;
    try {
      stat = await lstat(path);
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      hash.update('missing');
      return;
    }
    if (stat.isSymbolicLink()) {
      // Do not traverse an arbitrary external tree through config symlinks.
      hash.update(`symlink:${await readlink(path)}`);
    } else if (stat.isDirectory()) {
      hash.update('directory');
      for (const name of (await readdir(path)).sort()) await visit(join(path, name));
    } else if (stat.isFile()) {
      hash.update(`file:${stat.size}:`);
      hash.update(await readFile(path));
    } else {
      hash.update(`special:${stat.mode}`);
    }
  }
  for (const path of [...paths].sort()) await visit(path);
  return hash.digest('hex');
}

export async function runIsolated(command, args, {
  env = process.env,
  protectedPaths = protectedConfigPaths(env),
  stdio = 'inherit',
} = {}) {
  if (!command) throw new Error('Usage: node scripts/test-isolated.mjs <command> [args...]');
  const before = await configDigest(protectedPaths);
  const root = await mkdtemp(join(tmpdir(), 'esi-test-'));
  const childEnv = {
    ...env,
    ESI_STUDIO_TEST_ROOT: root,
    GOOSE_PATH_ROOT: root,
    GOOSE_DISABLE_KEYRING: '1',
    GOOSE_ADDITIONAL_CONFIG_FILES: '',
    GOOSE_TEST_SYSTEM_CONFIG_PATH: join(root, 'system/config.yaml'),
    XDG_CONFIG_HOME: join(root, 'xdg/config'),
    XDG_DATA_HOME: join(root, 'xdg/data'),
    XDG_STATE_HOME: join(root, 'xdg/state'),
    XDG_CACHE_HOME: join(root, 'xdg/cache'),
    APPDATA: join(root, 'appdata/roaming'),
    LOCALAPPDATA: join(root, 'appdata/local'),
    TMPDIR: join(root, 'tmp'),
    TMP: join(root, 'tmp'),
    TEMP: join(root, 'tmp'),
  };
  delete childEnv.PLUGINS;
  let exitCode;
  try {
    await mkdir(childEnv.TMPDIR);
    exitCode = await new Promise((accept, reject) => {
      const child = spawn(command, args, { env: childEnv, stdio, shell: false });
      const onInterrupt = () => child.kill('SIGINT');
      const onTerminate = () => child.kill('SIGTERM');
      process.on('SIGINT', onInterrupt);
      process.on('SIGTERM', onTerminate);
      const detach = () => {
        process.off('SIGINT', onInterrupt);
        process.off('SIGTERM', onTerminate);
      };
      child.once('error', (error) => { detach(); reject(error); });
      child.once('close', (code, signal) => {
        detach();
        accept(code ?? (signal === 'SIGINT' ? 130 : 143));
      });
    });
  } finally {
    const after = await configDigest(protectedPaths);
    if (before !== after) {
      // Never restore a snapshot over changes potentially made by a live user.
      throw new Error(`Protected Studio config changed; test root retained at ${root}`);
    }
    await rm(root, { recursive: true, force: true });
  }
  return exitCode;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const code = await runIsolated(process.argv[2], process.argv.slice(3));
    console.log(`Studio config digest unchanged; child exit ${code}`);
    process.exitCode = code;
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
