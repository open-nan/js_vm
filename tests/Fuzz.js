#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { Worker, isMainThread, parentPort, workerData } = require('node:worker_threads');
const log = require('./support/logger.js');
const {
  OPCODES,
  loadVm,
  makePrng,
  relative,
  runVmSource,
} = require('./support/vm_chain.js');
const {
  requireJsvuV8Path,
  resolveJsvuV8Path,
  v8EvalArgs,
  v8ReferencePrelude,
} = require('./support/reference_engine.js');
const { CORPUS_ROOT, readCorpusFiles, readCorpusCase } = require('./support/corpus.js');

const DEFAULT_FAILURE_DIR = path.join(__dirname, '..', 'artifacts/js_fuzzer/failures');
const DEFAULT_ISSUE_DIR = path.join(__dirname, '.issues');
const JS_FUZZER_VENDOR_ROOT = path.join(__dirname, '.vendor/js_fuzzer');
const DEFAULT_JS_FUZZER_DB_DIR = path.join(__dirname, 'db');
const DB_COVERAGE_MANIFEST = 'coverage.json';
const FUZZ_PROGRESS_INTERVAL_MS = 1000;
const ISSUE_LEVELS = {
  internalFailures: 1,
  runtimeTimeouts: 1,
  vmFailures: 1,
  compileErrors: 2,
  runtimeErrors: 3,
  differentialFailures: 3,
  expectedRuntimeErrors: 4,
  timeouts: 5,
  skipped: 5,
};
const ERROR_STATUSES = [
  'runtimeTimeouts',
  'vmFailures',
  'compileErrors',
  'runtimeErrors',
  'differentialFailures',
  'expectedRuntimeErrors',
  'internalFailures',
];
const BOUNDARY_VALUES = [
  0,
  1,
  -1,
  2,
  -2,
  2147483647,
  -2147483648,
  9007199254740991,
  -9007199254740991,
];

if (isMainThread) {
  const args = parseArgs(process.argv.slice(2));

  if (args.help) {
    printHelp();
    process.exit(0);
  }

  clearIssueHistoryIfRequested(args);
  const issueRecorder = args.replayIssues ? createNoopIssueRecorder() : createIssueRecorder(args);

  main(args, issueRecorder).then((exitCode) => {
    issueRecorder.close();
    if (exitCode) process.exit(exitCode);
  }).catch((err) => {
    log.error(err?.stack || err);
    issueRecorder.close();
    process.exit(err?.exitCode || 1);
  });
} else if (workerData?.mode === 'replayCase') {
  runReplayCaseWorker(workerData).then((result) => {
    parentPort.postMessage({ type: 'result', result });
  }).catch((err) => {
    parentPort.postMessage({
      type: 'error',
      error: err?.stack || String(err),
    });
  });
} else if (workerData?.mode === 'vmCaseRunner') {
  runVmCaseRunner(workerData).catch((err) => {
    parentPort.postMessage({
      type: 'error',
      error: err?.stack || String(err),
    });
  });
} else {
  runFuzzWorker(workerData).then(() => {
    parentPort.postMessage({ type: 'done', workerId: workerData.workerId });
  }).catch((err) => {
    parentPort.postMessage({
      type: 'error',
      workerId: workerData.workerId,
      error: err?.stack || String(err),
    });
  });
}

async function main(args, issueRecorder) {
  const mainStartedAt = Date.now();
  if (args.replayIssues) {
    return replayIssueRun(args, mainStartedAt);
  }

  const corpusSeeds = args.corpusSeeds ? loadCorpusSeeds() : [];
  if (args.differential) {
    args.referenceEnginePath = requireJsvuV8Path(args.referenceEnginePath);
  }
  const jsFuzzer = args.jsFuzzer ? createJsFuzzerAdapter(args) : disabledJsFuzzer();
  const totalCases = args.timeMs > 0 ? null : fuzzCaseCount(args, corpusSeeds);
  log.step(
    `Running js_fuzzer ${describeRunPlan(args, totalCases)}, threads=${args.threads}, caseLog=${args.caseLogMode}, seeds=${args.seeds}, baseSeed=${args.baseSeed}, maxBytes=${args.maxBytes}, errorLimit=${args.errorLimit || 'off'}, vmTimeout=${log.formatDuration(args.vmTimeoutMs)}, hostTimeout=${log.formatDuration(args.hostTimeoutMs)}, workerIdleCheck=${log.formatDuration(args.workerIdleCheckMs)}, workerStuck=${args.workerStuckMs ? log.formatDuration(args.workerStuckMs) : 'off'}, corpusSeeds=${corpusSeeds.length}, jsFuzzer=${jsFuzzer.available ? 'on' : 'off'}, differential=${args.differential ? `v8/${args.differentialStderr}` : 'off'}`,
  );
  if (!jsFuzzer.available) {
    log.warn(`V8 js_fuzzer disabled: ${jsFuzzer.reason}`);
  }

  const result = args.threads > 1
    ? await runParallelFuzz(args, issueRecorder, corpusSeeds, jsFuzzer, totalCases)
    : await runSingleThreadFuzz(args, issueRecorder, corpusSeeds, jsFuzzer, totalCases);
  const { stats, coverageSeen, checkedCases, fuzzStartedAt } = result;

  printFuzzSummary(args, jsFuzzer, stats, coverageSeen, checkedCases, fuzzStartedAt, mainStartedAt);

  if (isErrorLimitReached(args, stats)) return 1;
  if (stats.internalFailures > 0) return 1;
  if (stats.runtimeTimeouts > 0) return 1;
  if (stats.vmFailures > 0) return 1;
  if (stats.differentialFailures > 0) return 1;
  if (args.failOnCompileError && stats.compileErrors > 0) return 1;
  if (args.strictRuntime && stats.runtimeErrors > 0) return 1;
  return 0;
}

async function runSingleThreadFuzz(args, issueRecorder, corpusSeeds, jsFuzzer, totalCases) {
  log.step(`Loading JS VM wasm packages`);
  const vm = args.differential ? await createVmCaseRunner(args) : await loadVm();
  log.ok(`Loaded JS VM wasm packages`);

  const stats = createStats();
  const coverageSeen = new Set();
  const fuzzStartedAt = Date.now();
  const deadlineAt = args.timeMs > 0 ? fuzzStartedAt + args.timeMs : 0;
  const progressReporter = createProgressReporter();
  let checkedCases = 0;

  try {
    for (let index = 0; shouldRunNextCase(index, totalCases, deadlineAt); index += 1) {
      const generated = generateTask(index, args, corpusSeeds, jsFuzzer);
      const started = Date.now();
      const progress = fuzzProgressText(index + 1, totalCases, fuzzStartedAt, deadlineAt);

      if (generated.bytes > args.maxBytes) {
        const reason = `skipped: ${generated.bytes} exceeds ${args.maxBytes} bytes`;
        const result = failure('skipped', reason);
        recordFuzzResult({
          args,
          issueRecorder,
          stats,
          coverageSeen,
          generated,
          result,
          progress,
          durationMs: Date.now() - started,
        });
        checkedCases += 1;
        maybeLogAggregateProgress(progressReporter, args, checkedCases, fuzzStartedAt, deadlineAt);
        if (tripErrorLimit(args, stats)) break;
        continue;
      }

      const result = await runGeneratedSource(vm, generated, args);
      recordFuzzResult({
        args,
        issueRecorder,
        stats,
        coverageSeen,
        generated,
        result,
        progress,
        durationMs: Date.now() - started,
      });
      checkedCases += 1;
      maybeLogAggregateProgress(progressReporter, args, checkedCases, fuzzStartedAt, deadlineAt);
      if (tripErrorLimit(args, stats)) break;
    }
  } finally {
    if (args.differential) await vm.close();
  }

  return {
    stats,
    coverageSeen,
    checkedCases,
    fuzzStartedAt,
  };
}

async function replayIssueRun(args, mainStartedAt) {
  const replayStartedAt = Date.now();
  const issueDir = resolveIssueReplayDir(args.replayIssues, args.issueDir);
  const cases = collectIssueReplayCases(issueDir);
  if (!cases.length) {
    throw new Error(`no replayable issue sources found under ${relative(issueDir)}`);
  }

  log.step(
    `Replaying ${cases.length} issue case(s) from ${relative(issueDir)}, timeout=${log.formatDuration(args.replayTimeoutMs)}`,
  );

  const stats = createStats();
  const coverageSeen = new Set();
  const issueRecorder = createNoopIssueRecorder();

  for (let index = 0; index < cases.length; index += 1) {
    const replayCase = cases[index];
    const progress = log.progressText(index + 1, cases.length);
    const { result, durationMs, timedOut } = await runReplayCaseInWorker(args, replayCase);
    recordFuzzResult({
      args,
      issueRecorder,
      stats,
      coverageSeen,
      generated: replayCase,
      result,
      progress,
      durationMs,
      workerId: 0,
      saveArtifacts: false,
    });
    if (timedOut) {
      log.warn(`${replayCase.id} timed out after ${log.formatDuration(args.replayTimeoutMs)}`);
    }
  }

  printReplaySummary(args, stats, coverageSeen, cases.length, replayStartedAt, mainStartedAt);

  if (stats.internalFailures > 0) return 1;
  if (stats.runtimeTimeouts > 0) return 1;
  if (stats.vmFailures > 0) return 1;
  if (stats.differentialFailures > 0) return 1;
  if (args.failOnCompileError && stats.compileErrors > 0) return 1;
  if (args.strictRuntime && stats.runtimeErrors > 0) return 1;
  return 0;
}

function runReplayCaseInWorker(args, replayCase) {
  const started = Date.now();
  return new Promise((resolve) => {
    let settled = false;
    let timer = null;
    const worker = new Worker(__filename, {
      workerData: {
        mode: 'replayCase',
        args: {
          ...args,
          silentSetup: true,
        },
        task: replayCase,
      },
    });

    const finish = (payload) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      resolve({
        durationMs: Date.now() - started,
        timedOut: false,
        ...payload,
      });
    };

    timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      worker.terminate().catch((err) => {
        log.error(`failed to terminate replay worker for ${replayCase.id}: ${errorText(err)}`);
      });
      resolve({
        result: failure(
          'internalFailures',
          `replay timeout after ${log.formatDuration(args.replayTimeoutMs)}: ${replayCase.id}`,
        ),
        durationMs: Date.now() - started,
        timedOut: true,
      });
    }, args.replayTimeoutMs);
    timer.unref?.();

    worker.on('message', (message) => {
      if (message.type === 'result') {
        finish({ result: message.result.result, durationMs: message.result.durationMs });
      } else if (message.type === 'error') {
        finish({ result: failure('internalFailures', message.error) });
      }
    });
    worker.on('error', (err) => {
      finish({ result: failure('internalFailures', errorText(err)) });
    });
    worker.on('exit', (code) => {
      if (settled) return;
      if (code === 0) {
        finish({ result: failure('internalFailures', `replay worker exited without a result: ${replayCase.id}`) });
        return;
      }
      finish({ result: failure('internalFailures', `replay worker exited with code ${code}: ${replayCase.id}`) });
    });
  });
}

async function runParallelFuzz(args, issueRecorder, corpusSeeds, jsFuzzer, totalCases) {
  log.step(`Starting ${args.threads} fuzz worker threads`);
  const stats = createStats();
  const coverageSeen = new Set();
  let fuzzStartedAt = 0;
  let deadlineAt = 0;
  const progressReporter = createProgressReporter();
  const issueLogQueue = createAsyncLogQueue();
  const stopBuffer = new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT);
  const stopFlag = new Int32Array(stopBuffer);
  let checkedCases = 0;

  await new Promise((resolve, reject) => {
    let remaining = args.threads;
    let ready = 0;
    let settled = false;
    const workers = [];
    const liveWorkers = new Map();
    let stopReason = '';
    let stopTimer = null;
    let workerWatchTimer = null;

    function cleanupTimers() {
      if (stopTimer) clearTimeout(stopTimer);
      if (workerWatchTimer) clearInterval(workerWatchTimer);
    }

    function settle(fn, value) {
      if (settled) return;
      settled = true;
      cleanupTimers();
      process.off('SIGINT', handleInterrupt);
      process.off('SIGTERM', handleTerminate);
      fn(value);
    }

    function markWorkerSeen(workerId, messageType) {
      const state = liveWorkers.get(workerId);
      if (!state) return;
      state.lastSeenAt = Date.now();
      state.lastMessage = messageType;
    }

    function markWorkerCaseStart(workerId, generated, startedAt) {
      const state = liveWorkers.get(workerId);
      if (!state) return;
      state.currentCase = generated;
      state.currentCaseStartedAt = startedAt || Date.now();
      state.stuckHandled = false;
    }

    function markWorkerCaseDone(workerId) {
      const state = liveWorkers.get(workerId);
      if (!state) return;
      state.currentCase = null;
      state.currentCaseStartedAt = 0;
      state.stuckHandled = false;
    }

    function noteWorkerDone(workerId) {
      if (!liveWorkers.has(workerId)) return;
      liveWorkers.delete(workerId);
      remaining -= 1;
      if (remaining === 0) settle(resolve);
    }

    function requestStop(reason) {
      if (Atomics.exchange(stopFlag, 0, 1) === 0) {
        stopReason = reason;
        log.warn(`${reason}; waiting for ${liveWorkers.size} worker(s) to finish`);
      }
      startWorkerWatchdog();
    }

    function startWorkerWatchdog() {
      if (workerWatchTimer || args.workerIdleCheckMs <= 0) return;
      workerWatchTimer = setInterval(checkWorkerLiveness, args.workerIdleCheckMs);
      workerWatchTimer.unref?.();
    }

    function checkWorkerLiveness() {
      if (settled || liveWorkers.size === 0) return;
      const now = Date.now();
      for (const [workerId, state] of liveWorkers) {
        if (state.worker.threadId === -1) {
          log.warn(`worker#${workerId} has exited without a done message; marking it finished`);
          noteWorkerDone(workerId);
          continue;
        }
        if (state.terminating) continue;
        const idleMs = now - state.lastSeenAt;
        if (idleMs < args.workerIdleCheckMs) continue;
        if (now - state.lastReportAt < args.workerIdleCheckMs) continue;
        state.lastReportAt = now;
        const currentCase = formatWorkerCurrentCase(state, now);
        log.warn(
          `worker#${workerId} still running; no worker message for ${log.formatDuration(idleMs)} after stop` +
            ` (${stopReason || 'stop requested'}), threadId=${state.worker.threadId}, last=${state.lastMessage}` +
            currentCase,
        );
        if (isWorkerCaseStuck(args, state, now)) {
          archiveAndTerminateStuckWorker(workerId, state, now);
        }
      }
    }

    function archiveAndTerminateStuckWorker(workerId, state, now) {
      state.stuckHandled = true;
      state.terminating = true;
      const runningMs = Math.max(0, now - state.currentCaseStartedAt);
      const generated = restoreGeneratedTaskFromSummary(state.currentCase, args, corpusSeeds, jsFuzzer);
      const reason =
        `worker#${workerId} stuck for ${log.formatDuration(runningMs)} ` +
        `on ${generated.id}; terminating stuck worker`;
      const nextChecked = checkedCases + 1;
      const progress = fuzzProgressText(nextChecked, totalCases, fuzzStartedAt || Date.now(), deadlineAt);
      recordFuzzResult({
        args,
        issueRecorder,
        logQueue: issueLogQueue,
        stats,
        coverageSeen,
        generated,
        result: failure('internalFailures', reason),
        progress,
        durationMs: runningMs,
        workerId,
      });
      checkedCases = nextChecked;
      state.worker.terminate().catch((err) => {
        log.error(`failed to terminate stuck worker#${workerId}: ${errorText(err)}`);
      });
    }

    function terminateWorkersForInterrupt(reason) {
      for (const [workerId, state] of liveWorkers) {
        state.worker.terminate().catch((err) => {
          log.error(`failed to terminate worker#${workerId} after ${reason}: ${errorText(err)}`);
        });
      }
    }

    function handleInterrupt() {
      Atomics.store(stopFlag, 0, 1);
      terminateWorkersForInterrupt('SIGINT');
      settle(reject, Object.assign(new Error('interrupted by SIGINT'), { exitCode: 130 }));
    }

    function handleTerminate() {
      Atomics.store(stopFlag, 0, 1);
      terminateWorkersForInterrupt('SIGTERM');
      settle(reject, Object.assign(new Error('interrupted by SIGTERM'), { exitCode: 143 }));
    }

    process.once('SIGINT', handleInterrupt);
    process.once('SIGTERM', handleTerminate);

    for (let workerId = 0; workerId < args.threads; workerId += 1) {
      const workerArgs = {
        ...args,
        jsFuzzer: args.jsFuzzer && jsFuzzer.available,
        rebuildJsFuzzerDb: false,
        silentSetup: true,
      };
      const worker = new Worker(__filename, {
        workerData: {
          args: workerArgs,
          corpusSeeds,
          workerId,
          threads: args.threads,
          totalCases,
          stopBuffer,
        },
      });
      workers.push(worker);
      liveWorkers.set(workerId, {
        worker,
        lastSeenAt: Date.now(),
        lastReportAt: 0,
        lastMessage: 'spawned',
        currentCase: null,
        currentCaseStartedAt: 0,
        stuckHandled: false,
        terminating: false,
      });

      worker.on('message', (message) => {
        if (settled) return;
        markWorkerSeen(message.workerId, message.type);
        if (message.type === 'ready') {
          ready += 1;
          if (ready === args.threads) {
            fuzzStartedAt = Date.now();
            deadlineAt = args.timeMs > 0 ? fuzzStartedAt + args.timeMs : 0;
            log.ok(`${args.threads} fuzz worker threads ready`);
            if (deadlineAt > 0) {
              stopTimer = setTimeout(() => {
                requestStop(`Time budget reached: ${log.formatDuration(args.timeMs)}`);
              }, Math.max(0, deadlineAt - Date.now()));
              stopTimer.unref?.();
            }
            for (const readyWorker of workers) {
              readyWorker.postMessage({ type: 'run', deadlineAt });
            }
          }
        } else if (message.type === 'caseStart') {
          markWorkerCaseStart(message.workerId, message.generated, message.startedAt);
        } else if (message.type === 'case') {
          const state = liveWorkers.get(message.workerId);
          if (state?.stuckHandled) return;
          markWorkerCaseDone(message.workerId);
          const nextChecked = checkedCases + 1;
          const progress = fuzzProgressText(nextChecked, totalCases, fuzzStartedAt, deadlineAt);
          recordFuzzResult({
            args,
            issueRecorder,
            logQueue: issueLogQueue,
            stats,
            coverageSeen,
            generated: message.generated,
            result: message.result,
            progress,
            durationMs: message.durationMs,
            workerId: message.workerId,
          });
          checkedCases = nextChecked;
          maybeLogAggregateProgress(progressReporter, args, checkedCases, fuzzStartedAt, deadlineAt);
          if (isErrorLimitReached(args, stats)) {
            requestStop(`Error fuse reached: ${fuzzErrorCount(stats)}/${args.errorLimit}`);
          }
        } else if (message.type === 'error') {
          settle(reject, new Error(`worker#${message.workerId} failed: ${message.error}`));
        } else if (message.type === 'done') {
          noteWorkerDone(message.workerId);
        }
      });

      worker.on('error', (err) => {
        if (settled) return;
        settle(reject, err);
      });
      worker.on('exit', (code) => {
        if (settled) return;
        const state = liveWorkers.get(workerId);
        if (state?.terminating) return noteWorkerDone(workerId);
        if (code === 0) return noteWorkerDone(workerId);
        if (code !== 0) {
          settle(reject, new Error(`worker#${workerId} exited with code ${code}`));
        }
      });
    }
  });
  await issueLogQueue.flush();

  return {
    stats,
    coverageSeen,
    checkedCases,
    fuzzStartedAt,
  };
}

async function runFuzzWorker(data) {
  const args = data.args;
  const jsFuzzer = args.jsFuzzer ? createJsFuzzerAdapter(args) : disabledJsFuzzer();
  const vm = args.differential ? await createVmCaseRunner(args) : await loadVm();
  const stopFlag = data.stopBuffer ? new Int32Array(data.stopBuffer) : null;
  parentPort.postMessage({ type: 'ready', workerId: data.workerId });
  const runMessage = await waitForWorkerRunMessage();

  try {
    for (
      let index = data.workerId;
      shouldRunNextCase(index, data.totalCases, runMessage.deadlineAt) && !isStopRequested(stopFlag);
      index += data.threads
    ) {
      const generated = generateTask(index, args, data.corpusSeeds, jsFuzzer);
      const started = Date.now();
      parentPort.postMessage({
        type: 'caseStart',
        workerId: data.workerId,
        generated: taskSummary(generated),
        startedAt: started,
      });

      let result;
      if (generated.bytes > args.maxBytes) {
        result = failure('skipped', `skipped: ${generated.bytes} exceeds ${args.maxBytes} bytes`);
      } else {
        result = await runGeneratedSource(vm, generated, args);
      }

      parentPort.postMessage({
        type: 'case',
        workerId: data.workerId,
        generated: shouldSendSourceForResult(result.status, args) ? generated : taskSummary(generated),
        result,
        durationMs: Date.now() - started,
      });
    }
  } finally {
    if (args.differential) await vm.close();
  }
}

async function runReplayCaseWorker(data) {
  const args = data.args;
  const task = data.task;
  const vm = args.differential ? await createVmCaseRunner(args) : await loadVm();
  const started = Date.now();
  try {
    const result = await runGeneratedSource(vm, task, args);
    return {
      result,
      durationMs: Date.now() - started,
    };
  } finally {
    if (args.differential) await vm.close();
  }
}

function waitForWorkerRunMessage() {
  return new Promise((resolve) => {
    parentPort.once('message', (message) => {
      if (message?.type === 'run') resolve(message);
    });
  });
}

async function runVmCaseRunner() {
  const vm = await loadVm();
  parentPort.postMessage({ type: 'ready' });
  parentPort.on('message', (message) => {
    if (message?.type === 'close') {
      parentPort.close();
      return;
    }
    if (message?.type !== 'runVmCase') return;

    const started = Date.now();
    const result = runVmExecutionResultLoaded(vm, message.task, message.args);
    parentPort.postMessage({
      type: 'result',
      requestId: message.requestId,
      result,
      durationMs: Date.now() - started,
    });
  });
}

async function createVmCaseRunner(args) {
  let worker = null;
  let readyPromise = null;
  let pending = null;
  let nextRequestId = 1;
  let closed = false;

  async function ensureWorker() {
    if (closed) throw new Error('VM case runner is closed');
    if (worker) {
      await readyPromise;
      return worker;
    }

    const nextWorker = new Worker(__filename, {
      workerData: {
        mode: 'vmCaseRunner',
      },
    });
    worker = nextWorker;

    readyPromise = new Promise((resolve, reject) => {
      let ready = false;

      function failPending(error) {
        if (!pending) return;
        const active = pending;
        pending = null;
        if (active.timer) clearTimeout(active.timer);
        active.resolve(makeVmRunnerInternalFailure(active.task, error, active.startedAt));
      }

      nextWorker.on('message', (message) => {
        if (message?.type === 'ready') {
          ready = true;
          resolve();
          return;
        }
        if (message?.type === 'error') {
          const error = message.error || 'VM case runner failed';
          if (!ready) reject(new Error(error));
          failPending(error);
          return;
        }
        if (message?.type !== 'result') return;
        if (!pending || message.requestId !== pending.requestId) return;
        const active = pending;
        pending = null;
        if (active.timer) clearTimeout(active.timer);
        active.resolve(message.result);
      });

      nextWorker.on('error', (err) => {
        if (!ready) reject(err);
        failPending(errorText(err));
      });

      nextWorker.on('exit', (code) => {
        if (worker === nextWorker) {
          worker = null;
          readyPromise = null;
        }
        if (pending) {
          failPending(`VM case runner exited with code ${code}`);
          return;
        }
        if (!ready && code !== 0) {
          reject(new Error(`VM case runner exited with code ${code}`));
        }
      });
    });

    await readyPromise;
    return nextWorker;
  }

  const runner = {
    async run(task) {
      const activeWorker = await ensureWorker();
      const startedAt = Date.now();
      const requestId = nextRequestId;
      nextRequestId += 1;

      return new Promise((resolve) => {
        const timeoutMs = args.vmTimeoutMs;
        const timer = setTimeout(() => {
          if (!pending || pending.requestId !== requestId) return;
          pending = null;
          const timedOutWorker = worker;
          worker = null;
          readyPromise = null;
          timedOutWorker?.terminate().catch((err) => {
            log.error(`failed to terminate VM case runner for ${task.id}: ${errorText(err)}`);
          });
          resolve(makeVmTimeoutExecution(task, args, startedAt));
        }, timeoutMs);
        timer.unref?.();

        pending = {
          requestId,
          task,
          startedAt,
          timer,
          resolve,
        };

        activeWorker.postMessage({
          type: 'runVmCase',
          requestId,
          task,
          args,
        });
      });
    },

    async close() {
      closed = true;
      if (pending?.timer) clearTimeout(pending.timer);
      pending = null;
      const activeWorker = worker;
      worker = null;
      readyPromise = null;
      if (!activeWorker) return;
      activeWorker.postMessage({ type: 'close' });
      await activeWorker.terminate().catch(() => {});
    },
  };

  await ensureWorker();
  return runner;
}

function makeVmTimeoutExecution(task, args, startedAt) {
  const timeoutText = log.formatDuration(args.vmTimeoutMs);
  const error = `VM runtime timeout after ${timeoutText}: ${task.id}`;
  return {
    kind: 'runtimeTimeouts',
    error,
    coverage: emptyCoverage(),
    execution: {
      engine: 'js-vm',
      stdout: '',
      stderr: normalizeProcessText(`TimeoutError: ${error}`),
      exitCode: null,
      signal: 'SIGTERM',
      timeout: true,
      durationMs: Date.now() - startedAt,
    },
  };
}

function makeVmRunnerInternalFailure(task, error, startedAt) {
  const text = errorText(error);
  return {
    kind: 'internalFailures',
    error: text,
    coverage: emptyCoverage(),
    execution: {
      engine: 'js-vm',
      stdout: '',
      stderr: normalizeProcessText(text),
      exitCode: 1,
      signal: null,
      timeout: false,
      durationMs: Date.now() - startedAt,
    },
  };
}

function printFuzzSummary(args, jsFuzzer, stats, coverageSeen, checkedCases, fuzzStartedAt, mainStartedAt) {
  const summaryRows = [
    [
      'Fuzz Cases',
      `${stats.ok + stats.expectedRuntimeErrors} passed, ${fuzzFailureCount(stats)} failed, ${stats.skipped} skipped, ${stats.timeouts} timeout, ${checkedCases} total`,
    ],
    ['Compile', `${stats.compileErrors} compile errors`],
    ['VM Failures', `${stats.vmFailures} vm-only failures`],
    [
      'Runtime',
      `${stats.runtimeErrors} unexpected, ${stats.expectedRuntimeErrors} expected JS errors`,
    ],
    ['VM Timeout', `${stats.runtimeTimeouts} runtime timeouts`],
    ['Differential', `${stats.differentialFailures} mismatches`],
    ['Both Timeout', `${stats.timeouts} both engines timed out`],
    ['Coverage', `${coverageSeen.size}/${OPCODES.length} opcodes`],
    ['Duplicates', duplicateSummary(stats, checkedCases)],
    ['Workers', workerSummary(stats)],
    ['Generator', jsFuzzer.available ? 'v8 js_fuzzer' : 'built-in fallback'],
    ['Total Time', log.formatDuration(Date.now() - mainStartedAt)],
  ];
  if (args.errorLimit > 0) {
    summaryRows.push([
      'Error Fuse',
      `${fuzzErrorCount(stats)}/${args.errorLimit}${isErrorLimitReached(args, stats) ? ' reached' : ''}`,
    ]);
  }
  if (args.timeMs > 0) {
    summaryRows.push([
      'Time',
      `${log.formatDuration(Date.now() - fuzzStartedAt)} elapsed / ${log.formatDuration(args.timeMs)} budget`,
    ]);
  }
  log.summary(summaryRows);
  log.finish(
    `${checkedCases} js_fuzzer programs checked: ` +
      `${stats.ok} ok, ` +
      `${stats.vmFailures} vm-only failures, ` +
      `${stats.compileErrors} compile errors, ` +
      `${stats.runtimeErrors} runtime errors, ` +
      `${stats.runtimeTimeouts} runtime timeouts, ` +
      `${stats.differentialFailures} differential mismatches, ` +
      `${stats.expectedRuntimeErrors} expected runtime errors, ` +
      `${stats.internalFailures} internal failures, ` +
      `${stats.timeouts} timeouts, ` +
      `${stats.skipped} skipped, ` +
      `duplicates ${formatPercent(duplicateRate(stats, checkedCases))}`,
  );
}

function printReplaySummary(args, stats, coverageSeen, checkedCases, replayStartedAt, mainStartedAt) {
  log.summary([
    [
      'Replay Cases',
      `${stats.ok + stats.expectedRuntimeErrors} passed, ${fuzzFailureCount(stats)} failed, ${stats.skipped} skipped, ${stats.timeouts} timeout, ${checkedCases} total`,
    ],
    ['Compile', `${stats.compileErrors} compile errors`],
    ['VM Failures', `${stats.vmFailures} vm-only failures`],
    [
      'Runtime',
      `${stats.runtimeErrors} unexpected, ${stats.expectedRuntimeErrors} expected JS errors`,
    ],
    ['VM Timeout', `${stats.runtimeTimeouts} runtime timeouts`],
    ['Differential', `${stats.differentialFailures} mismatches`],
    ['Both Timeout', `${stats.timeouts} both engines timed out`],
    ['Coverage', `${coverageSeen.size}/${OPCODES.length} opcodes`],
    ['Duplicates', duplicateSummary(stats, checkedCases)],
    ['Workers', workerSummary(stats)],
    ['Replay Time', log.formatDuration(Date.now() - replayStartedAt)],
    ['Total Time', log.formatDuration(Date.now() - mainStartedAt)],
  ]);
  log.finish(
    `${checkedCases} issue case(s) replayed: ` +
      `${stats.ok} ok, ` +
      `${stats.vmFailures} vm-only failures, ` +
      `${stats.compileErrors} compile errors, ` +
      `${stats.runtimeErrors} runtime errors, ` +
      `${stats.runtimeTimeouts} runtime timeouts, ` +
      `${stats.differentialFailures} differential mismatches, ` +
      `${stats.expectedRuntimeErrors} expected runtime errors, ` +
      `${stats.internalFailures} internal failures, ` +
      `${stats.timeouts} timeouts, ` +
      `${stats.skipped} skipped, ` +
      `duplicates ${formatPercent(duplicateRate(stats, checkedCases))}`,
  );
}

function createStats() {
  return {
    ok: 0,
    vmFailures: 0,
    compileErrors: 0,
    runtimeErrors: 0,
    differentialFailures: 0,
    expectedRuntimeErrors: 0,
    internalFailures: 0,
    runtimeTimeouts: 0,
    timeouts: 0,
    skipped: 0,
    sourceHashes: new Set(),
    duplicateCases: 0,
    workerCounts: new Map(),
  };
}

function fuzzFailureCount(stats) {
  return (
    stats.internalFailures +
    stats.runtimeTimeouts +
    stats.vmFailures +
    stats.differentialFailures
  );
}

function recordWorkerCase(stats, workerId) {
  const key = workerId === null || workerId === undefined ? 'main' : `#${workerId}`;
  stats.workerCounts.set(key, (stats.workerCounts.get(key) || 0) + 1);
}

function workerSummary(stats) {
  if (!stats.workerCounts.size) return '-';
  return [...stats.workerCounts.entries()]
    .sort(([left], [right]) => compareWorkerKeys(left, right))
    .map(([worker, count]) => `${worker}=${count}`)
    .join(' ');
}

function compareWorkerKeys(left, right) {
  if (left === 'main') return -1;
  if (right === 'main') return 1;
  return Number(left.slice(1)) - Number(right.slice(1));
}

function recordGeneratedIdentity(stats, generated) {
  if (!generated?.md5) return;
  if (stats.sourceHashes.has(generated.md5)) {
    stats.duplicateCases += 1;
    return;
  }
  stats.sourceHashes.add(generated.md5);
}

function duplicateRate(stats, checkedCases) {
  if (checkedCases <= 0) return 0;
  return stats.duplicateCases / checkedCases;
}

function duplicateSummary(stats, checkedCases) {
  const uniqueCases = stats.sourceHashes.size;
  return `${stats.duplicateCases}/${checkedCases} (${formatPercent(duplicateRate(stats, checkedCases))}), unique=${uniqueCases}`;
}

function formatWorkerCurrentCase(state, now = Date.now()) {
  if (!state.currentCase) return '';
  const task = state.currentCase;
  const runningMs = state.currentCaseStartedAt ? now - state.currentCaseStartedAt : 0;
  return (
    `, running=${task.id}` +
    ` md5=${task.md5}` +
    ` bytes=${task.bytes}` +
    ` for=${log.formatDuration(runningMs)}` +
    ` origin=${truncateText(task.origin || '-', 140)}`
  );
}

function isWorkerCaseStuck(args, state, now = Date.now()) {
  if (!args.workerStuckMs || args.workerStuckMs <= 0) return false;
  if (!state.currentCase || !state.currentCaseStartedAt || state.stuckHandled) return false;
  return now - state.currentCaseStartedAt >= args.workerStuckMs;
}

function restoreGeneratedTaskFromSummary(summary, args, corpusSeeds, jsFuzzer) {
  if (summary && Number.isInteger(summary.index) && summary.index >= 0) {
    const restored = generateTask(summary.index, args, corpusSeeds, jsFuzzer);
    if (restored.md5 === summary.md5) return restored;
    return diagnosticTaskFromSummary(
      summary,
      `failed to restore stuck fuzz case: expected md5 ${summary.md5}, got ${restored.md5}`,
    );
  }
  return diagnosticTaskFromSummary(summary, 'failed to restore stuck fuzz case: missing generated case index');
}

function diagnosticTaskFromSummary(summary = {}, reason) {
  const source = [
    '// Fuzz runner could not restore the original stuck case.',
    `// Reason: ${reason}`,
    `// Case: ${summary.id || '-'}`,
    `// Original md5: ${summary.md5 || '-'}`,
    `// Origin: ${summary.origin || '-'}`,
    '',
  ].join('\n');
  const md5 = log.md5Text(source);
  return {
    index: summary.index ?? -1,
    id: `unrestored-stuck-${md5.slice(0, 12)}.js`,
    source,
    md5,
    bytes: Buffer.byteLength(source),
    template: 'stuck-diagnostic',
    origin: summary.origin || 'fuzz-runner',
    seed: summary.seed || 0,
    expected: summary.expected,
  };
}

function truncateText(value, maxLength) {
  const text = String(value);
  if (text.length <= maxLength) return text;
  return `${text.slice(0, Math.max(0, maxLength - 3))}...`;
}

function formatPercent(rate) {
  return `${(rate * 100).toFixed(2)}%`;
}

function fuzzErrorCount(stats) {
  return ERROR_STATUSES.reduce((total, status) => total + (stats[status] || 0), 0);
}

function isErrorLimitReached(args, stats) {
  return args.errorLimit > 0 && fuzzErrorCount(stats) >= args.errorLimit;
}

function tripErrorLimit(args, stats, stopFlag = null) {
  if (!isErrorLimitReached(args, stats)) return false;

  if (stopFlag) {
    if (Atomics.exchange(stopFlag, 0, 1) === 0) {
      log.warn(`Error fuse reached: ${fuzzErrorCount(stats)}/${args.errorLimit}; stopping workers`);
    }
    return true;
  }

  log.warn(`Error fuse reached: ${fuzzErrorCount(stats)}/${args.errorLimit}; stopping fuzz`);
  return true;
}

function isStopRequested(stopFlag) {
  return stopFlag && Atomics.load(stopFlag, 0) === 1;
}

function taskSummary(task) {
  return {
    index: task.index,
    id: task.id,
    md5: task.md5,
    bytes: task.bytes,
    template: task.template,
    origin: task.origin,
    seed: task.seed,
    expected: task.expected,
    referenceExpected: task.referenceExpected,
  };
}

function recordFuzzResult({
  args,
  issueRecorder,
  logQueue = null,
  stats,
  coverageSeen,
  generated,
  result,
  progress,
  durationMs,
  workerId = null,
  saveArtifacts = true,
  countWorker = true,
}) {
  if (countWorker) recordWorkerCase(stats, workerId);
  recordGeneratedIdentity(stats, generated);
  stats[result.status] += 1;

  const newOpcodes = mergeCoverage(coverageSeen, result.coverage);
  const thread = workerId === null ? '' : ` worker#${workerId}`;
  if (logQueue) {
    if (shouldQueueIssueLog(result.status, args)) {
      logQueue.enqueue(() => {
        writeIssueResultLog({
          args,
          issueRecorder,
          generated,
          result,
          progress,
          durationMs,
          workerId,
          newOpcodes,
          coverageSize: coverageSeen.size,
          saveArtifacts,
        });
      });
    }
    return;
  }

  const shouldPrintCase = shouldPrintCaseLog(args, result.status);
  let saved = '';

  if (saveArtifacts && shouldSave(result.status, args)) {
    saved = saveFailure(generated, result, args.saveFailures);
    log.error(result.error);
  } else if (result.status !== 'ok' && args.verbose && !shouldPrintCase) {
    log.info(`${result.status} ${generated.id}: ${firstLine(result.error)}`);
  }

  issueRecorder.record(generated, result, {
    progress,
    durationMs,
  });

  if (shouldPrintCase) {
    const savedText = saved ? ` saved=${relative(saved)}` : '';
    log.jest(
      statusLabel(result.status, args),
      generated.id,
      `${progress}${thread} md5=${generated.md5} bytes=${generated.bytes} template=${generated.template} origin=${generated.origin} status=${result.status} obs=${result.observableHash || '-'} coverage=+${newOpcodes.length}/${coverageSeen.size}/${OPCODES.length} time=${log.formatDuration(
        durationMs,
      )}${savedText}`,
    );
  }
}

function writeIssueResultLog({
  args,
  issueRecorder,
  generated,
  result,
  progress,
  durationMs,
  workerId = null,
  newOpcodes = [],
  coverageSize = 0,
  saveArtifacts = true,
}) {
  const thread = workerId === null ? '' : ` worker#${workerId}`;
  let saved = '';
  if (saveArtifacts && shouldSave(result.status, args)) {
    saved = saveFailure(generated, result, args.saveFailures);
  }

  issueRecorder.record(generated, result, {
    progress,
    durationMs,
  });

  const savedText = saved ? ` saved=${relative(saved)}` : '';
  log.jest(
    statusLabel(result.status, args),
    generated.id,
    `${progress}${thread} md5=${generated.md5} bytes=${generated.bytes} template=${generated.template} origin=${generated.origin} status=${result.status} obs=${result.observableHash || '-'} coverage=+${newOpcodes.length}/${coverageSize}/${OPCODES.length} time=${log.formatDuration(
      durationMs,
    )}${savedText}`,
  );
  if (result.error) log.error(result.error);
}

function shouldPrintCaseLog(args, status) {
  if (args.caseLogMode === 'all') return true;
  if (args.caseLogMode === 'failures') return status !== 'ok';
  return false;
}

function shouldQueueIssueLog(status, args) {
  if (status === 'ok') return false;
  if (status === 'skipped') return shouldArchiveIssue(status, args.errorLevel);
  return true;
}

function shouldSendSourceForResult(status, args) {
  return shouldSave(status, args) || shouldArchiveIssue(status, args.errorLevel);
}

function createProgressReporter() {
  return {
    nextAt: 0,
  };
}

function createAsyncLogQueue() {
  const queue = [];
  const flushWaiters = [];
  let draining = false;

  function scheduleDrain() {
    if (!draining) {
      draining = true;
      setImmediate(drain);
    }
  }

  function drain() {
    while (queue.length) {
      const write = queue.shift();
      try {
        write();
      } catch (err) {
        log.error(err?.stack || err);
      }
    }
    draining = false;
    while (flushWaiters.length) flushWaiters.shift()();
  }

  return {
    enqueue(write) {
      queue.push(write);
      scheduleDrain();
    },
    flush() {
      if (!queue.length && !draining) return Promise.resolve();
      return new Promise((resolve) => {
        flushWaiters.push(resolve);
        scheduleDrain();
      });
    },
  };
}

function maybeLogAggregateProgress(
  reporter,
  args,
  checkedCases,
  fuzzStartedAt,
  deadlineAt,
) {
  if (args.caseLogMode === 'all' && args.threads <= 1) return;
  if (checkedCases === 0) return;
  const now = Date.now();
  if (now < reporter.nextAt) return;
  reporter.nextAt = now + FUZZ_PROGRESS_INTERVAL_MS;

  const elapsedMs = Math.max(1, now - fuzzStartedAt);
  const rate = checkedCases / (elapsedMs / 1000);
  const timeText = deadlineAt > 0
    ? `elapsed=${log.formatDuration(elapsedMs)} left=${log.formatDuration(Math.max(0, deadlineAt - now))}`
    : `elapsed=${log.formatDuration(elapsedMs)}`;
  log.jest(
    'CASE',
    `${checkedCases} checked`,
    `${timeText} rate=${rate.toFixed(1)}/s`,
  );
}

function loadCorpusSeeds() {
  return readCorpusFiles().map((file) => {
    const test = readCorpusCase(file);
    const md5 = log.md5Text(test.rawSource);
    return {
      file,
      name: relative(file),
      md5,
      seed: hash32(md5),
      source: test.source,
      expected: test.meta.expect,
    };
  });
}

function resolveIssueReplayDir(value, issueRoot) {
  const text = String(value || '').trim();
  if (!text) throw new Error('--replay-issues requires an issue run timestamp or directory');

  const direct = path.resolve(text);
  if (fs.existsSync(direct)) return direct;

  const underIssueRoot = path.resolve(issueRoot, text);
  if (fs.existsSync(underIssueRoot)) return underIssueRoot;

  throw new Error(`issue replay directory not found: ${text} (looked under ${relative(issueRoot)})`);
}

function collectIssueReplayCases(issueDir) {
  const files = walkIssueSourceFiles(issueDir).sort();
  return files.map((file, index) => issueFileToReplayCase(file, index));
}

function walkIssueSourceFiles(dir) {
  if (!fs.existsSync(dir)) return [];
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const file = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...walkIssueSourceFiles(file));
    } else if (entry.isFile() && isReplaySourceFile(file)) {
      files.push(file);
    }
  }
  return files;
}

function isReplaySourceFile(file) {
  if (file.endsWith('.json')) return false;
  if (path.basename(file) === 'log.txt' || path.basename(file) === 'errors.log') return false;
  return /\.(?:js|mjs|cjs|jsx|ts|tsx|vue)$/i.test(file);
}

function issueFileToReplayCase(file, index) {
  const source = fs.readFileSync(file, 'utf8');
  const meta = readIssueMeta(file);
  const md5 = log.md5Text(source);
  if (meta.md5 && meta.md5 !== md5) {
    log.warn(`${relative(file)} md5 drift: meta=${meta.md5}, source=${md5}`);
  }
  return {
    index,
    id: path.basename(file),
    source,
    md5,
    bytes: Buffer.byteLength(source),
    template: meta.template || path.basename(path.dirname(file)),
    origin: meta.origin || relative(file),
    seed: Number.isFinite(meta.seed) ? meta.seed : 0,
    expected: meta.expected,
    referenceExpected: meta.referenceExpected || meta.expectedResult,
    issueStatus: meta.status || path.basename(path.dirname(file)),
    issueFile: file,
  };
}

function readIssueMeta(file) {
  const metaFile = `${file}.json`;
  if (!fs.existsSync(metaFile)) return {};
  try {
    return JSON.parse(fs.readFileSync(metaFile, 'utf8'));
  } catch (err) {
    log.warn(`failed to parse issue metadata ${relative(metaFile)}: ${firstLine(errorText(err))}`);
    return {};
  }
}

function fuzzCaseCount(args, corpusSeeds) {
  const baseCount = args.corpusSeeds ? Math.max(corpusSeeds.length, 1) : 1;
  return baseCount * args.r;
}

function describeRunPlan(args, totalCases) {
  if (args.timeMs > 0) {
    return `time=${log.formatDuration(args.timeMs)}`;
  }
  return `r=${args.r}, total=${totalCases}`;
}

function shouldRunNextCase(index, totalCases, deadlineAt) {
  if (deadlineAt > 0) return Date.now() < deadlineAt;
  return index < totalCases;
}

function fuzzProgressText(current, totalCases, startedAt, deadlineAt) {
  if (deadlineAt <= 0) return log.progressText(current, totalCases);
  const elapsed = Math.max(0, Date.now() - startedAt);
  const budget = Math.max(1, deadlineAt - startedAt);
  const remaining = Math.max(0, deadlineAt - Date.now());
  return `[${current} cases ${log.formatDuration(elapsed)}/${log.formatDuration(budget)} left=${log.formatDuration(remaining)}]`;
}

function createJsFuzzerAdapter(args) {
  if (!fs.existsSync(path.join(JS_FUZZER_VENDOR_ROOT, 'script_mutator.js'))) {
    return disabledJsFuzzer(`missing ${relative(JS_FUZZER_VENDOR_ROOT)}`);
  }

  let scriptMutator;
  let sourceHelpers;
  let mutateDb;
  try {
    scriptMutator = require(path.join(JS_FUZZER_VENDOR_ROOT, 'script_mutator.js'));
    sourceHelpers = require(path.join(JS_FUZZER_VENDOR_ROOT, 'source_helpers.js'));
    mutateDb = require(path.join(JS_FUZZER_VENDOR_ROOT, 'db.js'));
  } catch (err) {
    return disabledJsFuzzer(`failed to load vendor modules: ${firstLine(errorText(err))}`);
  }

  const corpus = new sourceHelpers.BaseCorpus(CORPUS_ROOT);
  const corpusEntries = collectCorpusEntries();
  const relPaths = corpusEntries.map((entry) => entry.relPath);
  const sourceLoader = createVmCorpusSourceLoader(sourceHelpers, corpus);
  if (!relPaths.length) {
    return disabledJsFuzzer(`no corpus inputs under ${relative(CORPUS_ROOT)}`);
  }

  try {
    ensureJsFuzzerDb(args.jsFuzzerDb, args.rebuildJsFuzzerDb, mutateDb, corpusEntries, sourceLoader);
  } catch (err) {
    return disabledJsFuzzer(`failed to build mutation db: ${firstLine(errorText(err))}`);
  }
  if (!args.silentSetup) {
    log.info(`js_fuzzer DB covers ${corpusEntries.length} files from ${relative(CORPUS_ROOT)}`);
  }

  const settings = scriptMutator.defaultSettings();
  settings.engine = 'v8';
  settings.input_dir = CORPUS_ROOT;
  settings.no_of_files = 1;
  settings.diff_fuzz = false;
  settings.is_sandbox_fuzzing = false;
  settings.is_x64_linux = false;
  settings.CORRUPT_MEMORY = 0;
  settings.CORRUPT_MEMORY_VIA_WATCHPOINTS = 0;
  settings.ENABLE_ALLOCATION_TIMEOUT = 0;
  settings.MUTATE_ALLOCATION_TIMEOUT = 0;

  return {
    available: true,
    reason: '',
    generate(seed, index, corpusSeed) {
      try {
        const rand = makePrng(seed);
        const inputRelPaths = chooseJsFuzzerInputs(relPaths, rand, corpusSeed);
        const inputs = inputRelPaths
          .map((relPath) => safeLoadVmCorpusSource(sourceLoader, relPath))
          .filter(Boolean);
        if (!inputs.length) return null;

        const result = withDeterministicMathRandom(seed, () => {
          const mutator = new scriptMutator.ScriptMutator({ ...settings }, args.jsFuzzerDb);
          return mutator.mutateMultiple(inputs);
        });
        const source = sanitizeJsFuzzerSource(result.code);
        return {
          template: 'js_fuzzer',
          origin: `js_fuzzer:${inputRelPaths.join('+')}`,
          source,
          expected: undefined,
        };
      } catch (err) {
        if (args.verbose) {
          log.warn(`js_fuzzer generation failed for case ${index}: ${firstLine(errorText(err))}`);
        }
        return null;
      }
    },
  };
}

function disabledJsFuzzer(reason = 'disabled by --no-js-fuzzer') {
  return {
    available: false,
    reason,
    generate() {
      return null;
    },
  };
}

function collectCorpusEntries() {
  return readCorpusFiles().map((file) => {
    const rawSource = fs.readFileSync(file, 'utf8');
    return {
      relPath: path.relative(CORPUS_ROOT, file),
      md5: log.md5Text(rawSource),
      bytes: Buffer.byteLength(rawSource),
    };
  });
}

function ensureJsFuzzerDb(outputDir, rebuild, mutateDb, corpusEntries, sourceLoader) {
  const indexPath = path.join(outputDir, 'index.json');
  if (!rebuild && dbCoversCorpus(outputDir, corpusEntries)) return;

  fs.rmSync(outputDir, { recursive: true, force: true });
  fs.mkdirSync(outputDir, { recursive: true });
  const writer = new mutateDb.MutateDbWriter(outputDir);
  const files = [];
  const errors = [];
  for (const entry of corpusEntries) {
    let source;
    try {
      source = sourceLoader.load(entry.relPath);
    } catch (err) {
      errors.push(`${entry.relPath}: ${firstLine(errorText(err))}`);
      continue;
    }

    const before = writer.index.length;
    try {
      writer.process(source);
    } catch (err) {
      errors.push(`${entry.relPath}: ${firstLine(errorText(err))}`);
      continue;
    }
    let snippets = writer.index.length - before;
    let fallback = false;
    if (snippets === 0) {
      addFallbackDbSnippet(writer, outputDir, entry);
      snippets = 1;
      fallback = true;
    }
    files.push({
      ...entry,
      snippets,
      fallback,
    });
  }

  if (errors.length) {
    throw new Error(`mutation db coverage failed: ${errors.join('; ')}`);
  }
  writer.writeIndex();

  const index = JSON.parse(fs.readFileSync(indexPath, 'utf8'));
  if (!index.length) {
    throw new Error('mutation db is empty');
  }
  writeDbCoverageManifest(outputDir, corpusEntries, files, index);
}

function dbCoversCorpus(outputDir, corpusEntries) {
  const indexPath = path.join(outputDir, 'index.json');
  const manifestPath = path.join(outputDir, DB_COVERAGE_MANIFEST);
  if (!fs.existsSync(indexPath) || !fs.existsSync(manifestPath)) return false;
  let manifest;
  try {
    manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  } catch {
    return false;
  }
  if (manifest.version !== 1) return false;
  if (manifest.corpusRoot !== relative(CORPUS_ROOT)) return false;
  const expected = corpusEntries.map(corpusCoverageKey);
  const actual = Array.isArray(manifest.files) ? manifest.files.map(corpusCoverageKey) : [];
  if (expected.length !== actual.length) return false;
  return expected.every((entry, index) => entry === actual[index]);
}

function corpusCoverageKey(entry) {
  return `${entry.relPath}:${entry.md5}:${entry.bytes}`;
}

function writeDbCoverageManifest(outputDir, corpusEntries, files, index) {
  fs.writeFileSync(
    path.join(outputDir, DB_COVERAGE_MANIFEST),
    JSON.stringify(
      {
        version: 1,
        corpusRoot: relative(CORPUS_ROOT),
        totalFiles: corpusEntries.length,
        totalSnippets: index.length,
        files,
      },
      null,
      2,
    ),
  );
}

function addFallbackDbSnippet(writer, outputDir, entry) {
  const dirPath = path.join(outputDir, 'NumericLiteral');
  fs.mkdirSync(dirPath, { recursive: true });
  const filePath = path.join(dirPath, `${entry.md5.slice(0, 8)}.json`);
  const value = Number.parseInt(entry.md5.slice(0, 8), 16) % 100000;
  fs.writeFileSync(
    filePath,
    JSON.stringify({
      type: 'NumericLiteral',
      source: String(value),
      path: entry.relPath,
      originalPath: entry.relPath,
      dependencies: [],
    }),
  );
  writer.index.push({
    path: path.relative(outputDir, filePath),
    super: false,
  });
}

function createVmCorpusSourceLoader(sourceHelpers, corpus) {
  let parser;
  let generator;
  try {
    parser = require(path.join(JS_FUZZER_VENDOR_ROOT, 'node_modules/@babel/parser'));
    generator = require(path.join(JS_FUZZER_VENDOR_ROOT, 'node_modules/@babel/generator')).default;
  } catch (err) {
    throw new Error(`failed to load Babel parser for corpus fallback: ${firstLine(errorText(err))}`);
  }

  return {
    load(relPath) {
      let vendorError;
      try {
        const source = sourceHelpers.loadSource(corpus, relPath);
        if (source) return source;
      } catch (err) {
        vendorError = err;
      }

      try {
        return loadFallbackCorpusSource(parser, generator, corpus, relPath);
      } catch (err) {
        if (vendorError) {
          throw new Error(`${firstLine(errorText(vendorError))}; fallback failed: ${firstLine(errorText(err))}`);
        }
        throw err;
      }
    },
  };
}

function loadFallbackCorpusSource(parser, generator, corpus, relPath) {
  const absPath = path.join(corpus.inputDir, relPath);
  const rawSource = fs.readFileSync(absPath, 'utf8');
  const ast = parser.parse(rawSource, fallbackParserOptions(relPath));
  return makeFallbackCorpusSource(generator, corpus, relPath, rawSource, ast);
}

function fallbackParserOptions(relPath) {
  const plugins = [
    'doExpressions',
    'explicitResourceManagement',
    'exportDefaultFrom',
    'importAttributes',
    'topLevelAwait',
  ];
  if (/\.(?:ts|tsx|vue\.ts|vue\.tsx)$/.test(relPath)) {
    plugins.push('typescript');
  }
  if (/\.(?:jsx|tsx|vue\.jsx|vue\.tsx)$/.test(relPath)) {
    plugins.push('jsx');
  }
  return {
    sourceType: 'unambiguous',
    allowReturnOutsideFunction: true,
    tokens: false,
    ranges: false,
    plugins,
  };
}

function makeFallbackCorpusSource(generator, corpus, relPath, rawSource, ast) {
  return {
    corpus,
    relPath,
    flags: corpus.loadFlags(relPath, rawSource),
    dependentPaths: [],
    ast,
    get absPath() {
      return path.join(corpus.inputDir, relPath);
    },
    get diffFuzzPath() {
      return relPath;
    },
    isSloppy() {
      return false;
    },
    isStrict() {
      return ast.program.directives.some(isStrictDirective);
    },
    generateNoStrict() {
      const allDirectives = ast.program.directives;
      ast.program.directives = allDirectives.filter((directive) => !isStrictDirective(directive));
      try {
        return generator(ast.program, { comments: true }).code;
      } finally {
        ast.program.directives = allDirectives;
      }
    },
    loadDependencies() {},
  };
}

function isStrictDirective(directive) {
  return directive.value && directive.value.value === 'use strict';
}

function safeLoadVmCorpusSource(sourceLoader, relPath) {
  try {
    return sourceLoader.load(relPath);
  } catch {
    return null;
  }
}

function chooseJsFuzzerInputs(relPaths, rand, corpusSeed) {
  const selected = [];
  const corpusRelPath = corpusSeed?.file ? path.relative(CORPUS_ROOT, corpusSeed.file) : '';
  if (corpusRelPath && relPaths.includes(corpusRelPath)) {
    selected.push(corpusRelPath);
  }
  const targetCount = Math.min(relPaths.length, 1 + (rand() % 3));
  while (selected.length < targetCount) {
    const relPath = relPaths[rand() % relPaths.length];
    if (!selected.includes(relPath)) selected.push(relPath);
  }
  return selected;
}

function withDeterministicMathRandom(seed, fn) {
  const rand = makePrng(seed);
  const original = Math.random;
  Math.random = () => rand() / 0x100000000;
  try {
    return fn();
  } finally {
    Math.random = original;
  }
}

function sanitizeJsFuzzerSource(source) {
  const normalized = normalizeJsFuzzerSource(source);
  return [
    'var print = globalThis.__vmPrint || function() {};',
    'function __v8Native() { return undefined; }',
    normalized,
    '',
  ].join('\n');
}

function normalizeJsFuzzerSource(source) {
  let output = '';
  for (let index = 0; index < source.length; index += 1) {
    if (source[index] === '%' && isV8NativeIntrinsic(source, index)) {
      output += '__v8Native';
      index += readIdentifierLength(source, index + 1);
      continue;
    }
    output += source[index];
  }
  return output;
}

function isV8NativeIntrinsic(source, percentIndex) {
  const nameLength = readIdentifierLength(source, percentIndex + 1);
  if (!nameLength) return false;
  let cursor = percentIndex + 1 + nameLength;
  while (/\s/.test(source[cursor] || '')) cursor += 1;
  if (source[cursor] !== '(') return false;

  let before = percentIndex - 1;
  while (before >= 0 && /[ \t]/.test(source[before])) before -= 1;
  if (before < 0 || '\n\r({[,;:?=+-*!~&|^<>/'.includes(source[before])) return true;
  return ['return', 'throw', 'void', 'delete', 'typeof', 'await', 'yield'].includes(
    readPreviousWord(source, before),
  );
}

function readIdentifierLength(source, start) {
  if (!/[A-Za-z_$]/.test(source[start] || '')) return 0;
  let cursor = start + 1;
  while (/[\w$]/.test(source[cursor] || '')) cursor += 1;
  return cursor - start;
}

function readPreviousWord(source, end) {
  let cursor = end;
  while (cursor >= 0 && /[A-Za-z_$]/.test(source[cursor])) cursor -= 1;
  return source.slice(cursor + 1, end + 1);
}

function generateTask(index, args, corpusSeeds, jsFuzzer) {
  const corpusSeed = corpusSeeds.length ? corpusSeeds[index % corpusSeeds.length] : null;
  const boundaryValue = args.boundarySeeds
    ? BOUNDARY_VALUES[index % BOUNDARY_VALUES.length]
    : index;
  const seed = mixSeed(args.baseSeed, index, corpusSeed?.seed || 0, hash32(String(boundaryValue)));
  const rand = makePrng(seed);
  const generated = generateProgram(rand, index, {
    corpusSeed,
    boundaryValue,
    jsFuzzer,
    seed,
  });
  const md5 = log.md5Text(generated.source);
  return {
    index,
    id: `${generated.template}-${md5.slice(0, 12)}.js`,
    source: generated.source,
    md5,
    bytes: Buffer.byteLength(generated.source),
    template: generated.template,
    origin: generated.origin,
    seed,
    expected: generated.expected,
  };
}

async function runGeneratedSource(vm, task, args) {
  if (args.differential || task.referenceExpected) {
    return runDifferentialGeneratedSource(vm, task, args);
  }

  try {
    const run = runVmSource(vm, task.source, {
      seeds: args.seeds,
      baseSeed: task.seed,
      id: task.id,
      expect: task.expected,
      coverage: true,
      captureLogs: true,
    });
    const observable = [...run.logs, String(run.lastResult)].join('\n');
    return {
      status: 'ok',
      error: '',
      coverage: run.coverage,
      observable,
      observableHash: log.md5Text(observable).slice(0, 12),
    };
  } catch (err) {
    const error = errorText(err);
    if (isInternalError(error)) return failure('internalFailures', error);
    if (isCompileError(error)) return failure('compileErrors', error);
    if (isExpectedVmRuntimeError(error)) {
      return failure('expectedRuntimeErrors', error);
    }
    if (isExpectedHostRuntimeError(task.source, args)) {
      return failure('expectedRuntimeErrors', error);
    }
    return failure('runtimeErrors', error);
  }
}

async function runDifferentialGeneratedSource(vm, task, args) {
  const actual = await runVmExecutionResult(vm, task, args);
  const expected = task.referenceExpected || runReferenceExecutionResult(task.source, args);
  if (expected.unavailable) {
    return failure('skipped', expected.unavailable);
  }
  if (actual.kind === 'internalFailures') {
    return executionFailure('internalFailures', actual.error, expected, actual.execution, actual.coverage);
  }
  const skipReason = differentialSkipReason(task.source, expected, args);
  if (skipReason) {
    return {
      ...failure('skipped', skipReason),
      referenceExpected: expected,
    };
  }
  if (actual.execution.timeout) {
    return classifyVmTimeoutResult(task, actual, expected, args);
  }
  if (expected.timeout) {
    const comparison = compareExecutionResults(expected, actual.execution, args);
    const observable = observableForResults(expected, actual.execution, args);
    return {
      ...failure(
        'differentialFailures',
        formatDifferentialError(task, comparison, expected, actual.execution),
      ),
      coverage: actual.coverage,
      observable,
      observableHash: log.md5Text(observable).slice(0, 12),
      referenceExpected: expected,
      actualResult: actual.execution,
      comparison,
    };
  }
  if (isVmFailureAgainstPassingReference(actual, expected)) {
    const observable = observableForResults(expected, actual.execution, args);
    return {
      ...failure('vmFailures', formatVmFailureError(task, actual, expected)),
      coverage: actual.coverage,
      observable,
      observableHash: log.md5Text(observable).slice(0, 12),
      referenceExpected: expected,
      actualResult: actual.execution,
    };
  }

  const comparison = compareExecutionResults(expected, actual.execution, args);
  const observable = observableForResults(expected, actual.execution, args);

  if (!comparison.ok) {
    return {
      ...failure('differentialFailures', formatDifferentialError(task, comparison, expected, actual.execution)),
      coverage: actual.coverage,
      observable,
      observableHash: log.md5Text(observable).slice(0, 12),
      referenceExpected: expected,
      actualResult: actual.execution,
      comparison,
    };
  }

  return {
    status: actual.kind === 'runtimeErrors' ? 'expectedRuntimeErrors' : 'ok',
    error: '',
    coverage: actual.coverage,
    observable,
    observableHash: log.md5Text(observable).slice(0, 12),
    referenceExpected: expected,
    actualResult: actual.execution,
    comparison,
  };
}

async function runVmExecutionResult(vm, task, args) {
  if (typeof vm.run === 'function') return vm.run(task);
  return runVmExecutionResultLoaded(vm, task, args);
}

function runVmExecutionResultLoaded(vm, task, args) {
  const started = Date.now();
  try {
    const run = runVmSource(vm, task.source, {
      seeds: args.seeds,
      baseSeed: task.seed,
      id: task.id,
      coverage: true,
      captureLogs: true,
    });
    return {
      kind: 'ok',
      error: '',
      coverage: run.coverage,
      execution: {
        engine: 'js-vm',
        stdout: processStdoutFromVmLogs(run.logEntries),
        stderr: processStderrFromVmLogs(run.logEntries),
        exitCode: 0,
        signal: null,
        timeout: false,
        durationMs: Date.now() - started,
      },
    };
  } catch (err) {
    const error = errorText(err);
    const kind = isInternalError(error)
      ? 'internalFailures'
      : isCompileError(error)
        ? 'compileErrors'
        : 'runtimeErrors';
    return {
      kind,
      error,
      coverage: emptyCoverage(),
      execution: {
        engine: 'js-vm',
        stdout: '',
        stderr: normalizeProcessText(error),
        exitCode: 1,
        signal: null,
        timeout: false,
        durationMs: Date.now() - started,
      },
    };
  }
}

function emptyCoverage() {
  return { opcodes: [], sections: [], maxByteLength: 0, artifacts: 0 };
}

function classifyVmTimeoutResult(task, actual, expected, args) {
  const observable = observableForResults(expected, actual.execution, args);
  if (expected.timeout) {
    return {
      ...failure(
        'timeouts',
        `TIMEOUT: VM and reference engine ${expected.engine} both exceeded their timeout budgets`,
      ),
      coverage: actual.coverage,
      observable: 'TIMEOUT',
      observableHash: log.md5Text('TIMEOUT').slice(0, 12),
      referenceExpected: expected,
      actualResult: actual.execution,
    };
  }
  return {
    ...failure('runtimeTimeouts', formatRuntimeTimeoutError(task, actual, expected, args)),
    coverage: actual.coverage,
    observable,
    observableHash: log.md5Text(observable).slice(0, 12),
    referenceExpected: expected,
    actualResult: actual.execution,
  };
}

function runReferenceExecutionResult(source, args) {
  const engine = 'v8';
  const command = args.referenceEnginePath || resolveJsvuV8Path();
  if (!command) {
    return {
      unavailable: `jsvu V8 reference engine is not available`,
    };
  }

  const started = Date.now();
  const result = spawnSync(command, v8EvalArgs(v8ReferencePrelude() + source, command), {
    encoding: 'utf8',
    timeout: args.hostTimeoutMs,
    maxBuffer: 1024 * 1024,
  });
  const timedOut = result.error?.code === 'ETIMEDOUT';
  return {
    engine,
    command,
    stdout: normalizeProcessText(result.stdout || ''),
    stderr: normalizeProcessText(result.stderr || referenceExecutionError(result, timedOut)),
    exitCode: Number.isInteger(result.status) ? result.status : timedOut ? null : 1,
    signal: result.signal || null,
    timeout: timedOut,
    durationMs: Date.now() - started,
  };
}

function differentialSkipReason(source, expected, args) {
  if (/\b(?:import|export)\b/.test(source)) {
    return `module syntax is skipped in jsvu V8 shell differential mode`;
  }
  if (isReferenceSyntaxUnsupported(expected)) {
    return `reference engine ${expected.engine} cannot parse this generated source`;
  }
  if (usesNonPortableHostExternals(source)) {
    return `source uses non-portable host externals`;
  }
  return '';
}

function isReferenceSyntaxUnsupported(expected) {
  if (expected.exitCode === 0 || expected.timeout) return false;
  return /^SyntaxError:/.test(stderrSummary(`${expected.stderr || ''}\n${expected.stdout || ''}`));
}

function usesNonPortableHostExternals(source) {
  return /\b(?:fetch|document|localStorage|sessionStorage|XMLHttpRequest|navigator|location)\b/.test(source);
}

function referenceExecutionError(result, timedOut) {
  if (!result.error) return '';
  if (timedOut) return `TimeoutError: reference engine timed out`;
  return `${result.error.name || 'Error'}: ${result.error.message}`;
}

function processStdoutFromVmLogs(entries) {
  return processTextFromVmLogs(entries, (level) => level !== 'warn' && level !== 'error');
}

function processStderrFromVmLogs(entries) {
  return processTextFromVmLogs(entries, (level) => level === 'warn' || level === 'error');
}

function processTextFromVmLogs(entries, include) {
  const lines = [];
  for (const entry of entries || []) {
    const level = String(entry.level || 'log');
    if (include(level)) lines.push(String(entry.message));
  }
  return lines.length ? `${lines.join('\n')}\n` : '';
}

function normalizeProcessText(value) {
  return String(value || '').replace(/\r\n/g, '\n');
}

function compareExecutionResults(expected, actual, args) {
  const expectedComparable = comparableExecutionResult(expected, args);
  const actualComparable = comparableExecutionResult(actual, args);
  const differences = [];
  for (const key of ['stdout', 'stderr', 'exitCode', 'signal', 'timeout']) {
    if (expectedComparable[key] !== actualComparable[key]) {
      differences.push({
        field: key,
        expected: expectedComparable[key],
        actual: actualComparable[key],
      });
    }
  }
  return {
    ok: differences.length === 0,
    stderrMode: args.differentialStderr,
    differences,
  };
}

function observableForResults(expected, actual, args) {
  return JSON.stringify(
    {
      expected: comparableExecutionResult(expected, args),
      actual: comparableExecutionResult(actual, args),
    },
    null,
    0,
  );
}

function isVmFailureAgainstPassingReference(actual, expected) {
  return ['compileErrors', 'runtimeErrors'].includes(actual.kind) && isSuccessfulExecution(expected);
}

function isSuccessfulExecution(result) {
  return !result.timeout && result.exitCode === 0 && !result.signal;
}

function formatRuntimeTimeoutError(task, actual, expected, args) {
  return [
    `VM runtime timeout: ${task.id}`,
    `vm exceeded ${log.formatDuration(args.vmTimeoutMs)} but reference=${expected.engine} completed in ${log.formatDuration(expected.durationMs || 0)}`,
    `vm=${JSON.stringify(comparableExecutionResult(actual.execution, args))}`,
    `reference=${JSON.stringify(comparableExecutionResult(expected, args))}`,
  ].join('\n');
}

function formatVmFailureError(task, actual, expected) {
  return [
    `VM failed while reference passed: ${task.id}`,
    `vm-kind=${actual.kind} reference=${expected.engine}`,
    firstLine(actual.error),
  ].join('\n');
}

function comparableExecutionResult(result, args) {
  return {
    stdout: result.stdout,
    stderr: comparableStderr(result.stderr, args.differentialStderr),
    exitCode: result.exitCode,
    signal: result.signal,
    timeout: Boolean(result.timeout),
  };
}

function comparableStderr(stderr, mode) {
  const text = normalizeProcessText(stderr).trimEnd();
  if (mode === 'ignore') return '';
  if (mode === 'exact') return text;
  return stderrSummary(text);
}

function stderrSummary(stderr) {
  if (!stderr) return '';
  const match = stderr.match(/\b(?:TypeError|RangeError|ReferenceError|SyntaxError|URIError|EvalError|Error):[^\n]*/);
  if (match) return match[0];
  return firstLine(stderr);
}

function formatDifferentialError(task, comparison, expected, actual) {
  const diffText = comparison.differences
    .map((diff) => `${diff.field}: expected=${JSON.stringify(diff.expected)} actual=${JSON.stringify(diff.actual)}`)
    .join('; ');
  return [
    `Differential mismatch: ${task.id}`,
    `reference=${expected.engine} vm=${actual.engine} stderr=${comparison.stderrMode}`,
    diffText,
  ].join('\n');
}

function executionFailure(status, error, expected, actual, coverage) {
  return {
    ...failure(status, error),
    coverage,
    referenceExpected: expected,
    actualResult: actual,
  };
}

function failure(status, error) {
  return {
    status,
    error,
    coverage: { opcodes: [], sections: [], maxByteLength: 0, artifacts: 0 },
    observable: '',
    observableHash: '',
  };
}

function generateProgram(rand, index, context) {
  if (context.corpusSeed && index % 9 === 0) {
    return {
      template: 'corpus',
      origin: context.corpusSeed.name,
      source: context.corpusSeed.source,
      expected: context.corpusSeed.expected,
    };
  }
  if (index % 7 === 0) {
    return {
      template: 'boundary',
      origin: `boundary:${context.boundaryValue}`,
      source: boundaryProgram(context.boundaryValue, rand),
      expected: undefined,
    };
  }

  if (context.jsFuzzer.available) {
    const mutated = context.jsFuzzer.generate(context.seed, index, context.corpusSeed);
    if (mutated) return mutated;
  }

  const templates = [
    ['arithmetic', arithmeticProgram],
    ['function', functionProgram],
    ['array-object', arrayObjectProgram],
    ['closure', closureProgram],
    ['block-scope', blockScopeProgram],
    ['loop', loopProgram],
    ['try-catch', tryCatchProgram],
    ['class', classProgram],
  ];
  const [template, generate] = templates[index % templates.length];
  return {
    template,
    origin: context.corpusSeed ? `seed:${context.corpusSeed.name}` : 'generated',
    source: generate(rand, index),
    expected: undefined,
  };
}

function arithmeticProgram(rand) {
  const a = int(rand, 1, 20);
  const b = int(rand, 1, 20);
  const c = int(rand, 1, 10);
  return [
    `const a = ${a};`,
    `const b = ${b};`,
    `const c = ${c};`,
    `const value = (a + b) * c - (b % c);`,
    'value;',
    '',
  ].join('\n');
}

function boundaryProgram(value, rand) {
  const delta = int(rand, 0, 3);
  return [
    `const value = ${numberLiteral(value)};`,
    `const delta = ${delta};`,
    'const normalized = value + delta - delta;',
    'normalized === value ? value : -999;',
    '',
  ].join('\n');
}

function functionProgram(rand, index) {
  const n = int(rand, 2, 8);
  return [
    'function fib(n) {',
    '  if (n < 2) return n;',
    '  return fib(n - 1) + fib(n - 2);',
    '}',
    'function add(a, b) {',
    '  return a + b;',
    '}',
    `const value = add(fib(${n}), ${index % 7});`,
    'value;',
    '',
  ].join('\n');
}

function arrayObjectProgram(rand) {
  const a = int(rand, 1, 9);
  const b = int(rand, 1, 9);
  const c = int(rand, 1, 9);
  return [
    `const items = [${a}, ${b}, ${c}];`,
    'const box = { first: items[0], second: items[1], third: items.length };',
    'box.first + box.second + box.third;',
    '',
  ].join('\n');
}

function closureProgram(rand) {
  const base = int(rand, 1, 30);
  const delta = int(rand, 1, 12);
  return [
    'function makeCounter(start) {',
    '  let value = start;',
    '  return function step(delta) {',
    '    value = value + delta;',
    '    return value;',
    '  };',
    '}',
    `const next = makeCounter(${base});`,
    `next(${delta}) + next(1);`,
    '',
  ].join('\n');
}

function blockScopeProgram(rand) {
  const outer = int(rand, 1, 10);
  const inner = int(rand, 11, 30);
  return [
    `let value = ${outer};`,
    '{',
    `  let value = ${inner};`,
    '  value = value + 1;',
    '}',
    'value + (typeof missingName === "undefined" ? 1 : 100);',
    '',
  ].join('\n');
}

function loopProgram(rand) {
  const count = int(rand, 2, 7);
  return [
    'let total = 0;',
    `for (let i = 0; i < ${count}; i = i + 1) {`,
    '  total = total + i;',
    '}',
    'total;',
    '',
  ].join('\n');
}

function tryCatchProgram(rand) {
  const value = int(rand, 1, 50);
  return [
    'let value = 0;',
    'try {',
    `  throw ${value};`,
    '} catch (err) {',
    '  value = err + 1;',
    '} finally {',
    '  value = value + 1;',
    '}',
    'value;',
    '',
  ].join('\n');
}

function classProgram(rand) {
  const value = int(rand, 1, 20);
  return [
    'class Box {',
    '  constructor(value) {',
    '    this.value = value;',
    '  }',
    '}',
    `const box = new Box(${value});`,
    'box.value;',
    '',
  ].join('\n');
}

function int(rand, min, max) {
  return min + (rand() % (max - min + 1));
}

function numberLiteral(value) {
  return Object.is(value, -0) ? '-0' : String(value);
}

function mergeCoverage(seen, coverage) {
  const next = [];
  for (const opcode of coverage.opcodes || []) {
    if (seen.has(opcode)) continue;
    seen.add(opcode);
    next.push(opcode);
  }
  return next;
}

function shouldSave(status, args) {
  if (status === 'internalFailures') return true;
  if (status === 'runtimeTimeouts') return true;
  if (status === 'vmFailures') return true;
  if (status === 'differentialFailures') return true;
  if (status === 'compileErrors') return args.saveCompileErrors;
  if (status === 'runtimeErrors') return args.saveRuntimeErrors || args.strictRuntime;
  return false;
}

function clearIssueHistoryIfRequested(args) {
  if (!args.clearIssues) return;

  const issueDir = path.resolve(args.issueDir);
  assertSafeIssueClearDir(issueDir);
  fs.rmSync(issueDir, { recursive: true, force: true });
  log.warn(`Cleared issue history: ${relative(issueDir)}`);
}

function assertSafeIssueClearDir(issueDir) {
  const forbidden = new Set([
    path.parse(issueDir).root,
    path.resolve(__dirname, '..'),
    path.resolve(__dirname),
    path.resolve(process.cwd()),
  ]);
  if (process.env.HOME) forbidden.add(path.resolve(process.env.HOME));

  if (forbidden.has(issueDir)) {
    throw new Error(`refusing to clear unsafe issue directory: ${issueDir}`);
  }

  const defaultIssueDir = path.resolve(DEFAULT_ISSUE_DIR);
  const isDefaultIssueDir = issueDir === defaultIssueDir || issueDir.startsWith(`${defaultIssueDir}${path.sep}`);
  const hasIssueName = path.basename(issueDir).toLowerCase().includes('issue');
  if (!isDefaultIssueDir && !hasIssueName) {
    throw new Error('--clear-issues requires --issue-dir to point at tests/.issues or a directory whose name contains "issue"');
  }
}

function createIssueRecorder(args) {
  const enabled = args.log || args.errorLevel > 0;
  if (!enabled) {
    return {
      record() {},
      close() {},
    };
  }

  const runIssueDir = path.join(args.issueDir, issueRunDirectoryName());
  fs.mkdirSync(runIssueDir, { recursive: true });
  const logFile = path.join(runIssueDir, 'log.txt');
  const errorFile = path.join(runIssueDir, 'errors.log');

  if (args.log) fs.writeFileSync(logFile, '');
  if (args.errorLevel > 0) fs.writeFileSync(errorFile, '');

  const originalLog = console.log.bind(console);
  const originalError = console.error.bind(console);

  if (args.log) {
    console.log = (...parts) => {
      appendLogLine(logFile, parts);
      originalLog(...parts);
    };
    console.error = (...parts) => {
      appendLogLine(logFile, parts);
      originalError(...parts);
    };
  }

  return {
    record(task, result, context = {}) {
      if (!shouldArchiveIssue(result.status, args.errorLevel)) return null;
      return saveIssue(task, result, runIssueDir, context, errorFile);
    },
    close() {
      console.log = originalLog;
      console.error = originalError;
    },
  };
}

function createNoopIssueRecorder() {
  return {
    record() {},
    close() {},
  };
}

function shouldArchiveIssue(status, errorLevel) {
  return (ISSUE_LEVELS[status] || 0) > 0 && ISSUE_LEVELS[status] <= errorLevel;
}

function saveIssue(task, result, outputDir, context, errorFile) {
  const hash = task.md5.slice(0, 12);
  const base = path.basename(task.id || 'generated.js').replace(/[^a-zA-Z0-9_.-]/g, '_');
  const statusDir = path.join(outputDir, result.status);
  fs.mkdirSync(statusDir, { recursive: true });

  const issueFile = path.join(statusDir, `${base}`);
  const metaFile = `${issueFile}.json`;
  fs.writeFileSync(issueFile, task.source);
  fs.writeFileSync(
    metaFile,
    `${JSON.stringify(
      {
        status: result.status,
        source: task.id,
        md5: task.md5,
        seed: task.seed,
        template: task.template,
        origin: task.origin,
        expected: task.expected,
        bytes: task.bytes,
        progress: context.progress,
        durationMs: context.durationMs,
        observable: result.observable,
        observableHash: result.observableHash,
        referenceExpected: result.referenceExpected,
        actualResult: result.actualResult,
        comparison: result.comparison,
        coverage: result.coverage,
        error: result.error,
        savedAt: new Date().toISOString(),
      },
      null,
      2,
    )}\n`,
  );
  if (result.referenceExpected) {
    fs.writeFileSync(
      `${issueFile}.expected.json`,
      `${JSON.stringify(result.referenceExpected, null, 2)}\n`,
    );
  }
  if (result.actualResult) {
    fs.writeFileSync(
      `${issueFile}.actual.json`,
      `${JSON.stringify(result.actualResult, null, 2)}\n`,
    );
  }

  if (errorFile) {
    fs.appendFileSync(
      errorFile,
      [
        `[${logTimestamp()}] ${result.status} ${task.id}`,
        `md5=${task.md5}`,
        `seed=${task.seed}`,
        `origin=${task.origin}`,
        `saved=${relative(issueFile)}`,
        `error=${firstLine(result.error || '-')}`,
      ].join(' ') + '\n',
    );
  }
  return issueFile;
}

function appendLogLine(file, parts) {
  fs.appendFileSync(file, `${stripAnsi(parts.map(String).join(' '))}\n`);
}

function stripAnsi(value) {
  return String(value).replace(/\x1B\[[0-?]*[ -/]*[@-~]/g, '');
}

function issueRunDirectoryName(date = new Date()) {
  const year = String(date.getFullYear()).slice(-2);
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${year}-${month}-${day}:${logTimestamp(date)}`;
}

function logTimestamp(date = new Date()) {
  return date.toTimeString().slice(0, 8);
}

function saveFailure(task, result, outputDir = DEFAULT_FAILURE_DIR) {
  const hash = task.md5.slice(0, 12);
  const base = path.basename(task.id || 'generated.js').replace(/[^a-zA-Z0-9_.-]/g, '_');
  fs.mkdirSync(outputDir, { recursive: true });

  const failureFile = path.join(outputDir, `${hash}-${base}`);
  const metaFile = `${failureFile}.json`;
  fs.writeFileSync(failureFile, task.source);
  fs.writeFileSync(
    metaFile,
    `${JSON.stringify(
      {
        source: task.id,
        md5: task.md5,
        seed: task.seed,
        template: task.template,
        origin: task.origin,
        expected: task.expected,
        referenceExpected: result.referenceExpected,
        actualResult: result.actualResult,
        comparison: result.comparison,
        observable: result.observable,
        observableHash: result.observableHash,
        coverage: result.coverage,
        error: result.error,
        savedAt: new Date().toISOString(),
      },
      null,
      2,
    )}\n`,
  );
  if (result.referenceExpected) {
    fs.writeFileSync(
      `${failureFile}.expected.json`,
      `${JSON.stringify(result.referenceExpected, null, 2)}\n`,
    );
  }
  if (result.actualResult) {
    fs.writeFileSync(
      `${failureFile}.actual.json`,
      `${JSON.stringify(result.actualResult, null, 2)}\n`,
    );
  }
  return failureFile;
}

function statusLabel(status, args) {
  if (status === 'ok') return 'PASS';
  if (status === 'expectedRuntimeErrors') return 'PASS';
  if (status === 'runtimeTimeouts') return 'TIMEOUT';
  if (status === 'timeouts') return 'TIMEOUT';
  if (status === 'skipped') return 'SKIP';
  if (status === 'internalFailures') return 'FAIL';
  if (status === 'vmFailures') return 'FAIL';
  if (status === 'differentialFailures') return 'FAIL';
  if (status === 'compileErrors') return args.failOnCompileError ? 'FAIL' : 'WARN';
  if (status === 'runtimeErrors') return args.strictRuntime ? 'FAIL' : 'WARN';
  return 'DONE';
}

function parseArgs(argv) {
  const parsed = {
    r: numberEnv('JS_VM_FUZZ_R', 4),
    threads: numberEnv('JS_VM_FUZZ_THREADS', 1),
    seeds: numberEnv('JS_VM_FUZZ_SEEDS', 2),
    baseSeed: numberEnv('JS_VM_RANDOM_BASE_SEED', 1337),
    timeMs: durationEnv('JS_VM_FUZZ_TIME', 0),
    maxBytes: numberEnv('JS_VM_FUZZ_MAX_BYTES', 8192),
    saveFailures: DEFAULT_FAILURE_DIR,
    issueDir: DEFAULT_ISSUE_DIR,
    replayIssues: '',
    replayTimeoutMs: numberEnv('JS_VM_FUZZ_REPLAY_TIMEOUT_MS', 30000),
    log: false,
    clearIssues: false,
    caseLog: 'auto',
    caseLogMode: 'all',
    errorLevel: numberEnv('JS_VM_FUZZ_ERROR_LEVEL', 0),
    errorLimit: numberEnv('JS_VM_FUZZ_ERROR_LIMIT', 0),
    saveCompileErrors: false,
    saveRuntimeErrors: false,
    strictRuntime: false,
    hostTimeoutMs: numberEnv('JS_VM_FUZZ_HOST_TIMEOUT_MS', 1000),
    vmTimeoutMs: nullableNumberEnv('JS_VM_FUZZ_VM_TIMEOUT_MS'),
    differential: boolEnv('JS_VM_FUZZ_DIFFERENTIAL', false),
    referenceEngine: stringEnv('JS_VM_FUZZ_REFERENCE_ENGINE', 'v8'),
    referenceEnginePath: stringEnv('JSVU_V8_PATH', stringEnv('JSVU_V8', '')),
    differentialStderr: stringEnv('JS_VM_FUZZ_DIFFERENTIAL_STDERR', 'summary'),
    workerIdleCheckMs: numberEnv(
      'JS_VM_FUZZ_WORKER_IDLE_CHECK_MS',
      numberEnv('JS_VM_FUZZ_WORKER_GRACE_MS', 5000),
    ),
    workerStuckMs: numberEnv('JS_VM_FUZZ_WORKER_STUCK_MS', 30000),
    failOnCompileError: false,
    corpusSeeds: true,
    boundarySeeds: true,
    jsFuzzer: true,
    jsFuzzerDb: DEFAULT_JS_FUZZER_DB_DIR,
    rebuildJsFuzzerDb: false,
    verbose: false,
    help: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help' || arg === '-h') parsed.help = true;
    else if (arg === '--verbose') parsed.verbose = true;
    else if (arg === '--strict-runtime') parsed.strictRuntime = true;
    else if (arg === '--fail-on-compile-error') parsed.failOnCompileError = true;
    else if (arg === '--save-compile-errors') parsed.saveCompileErrors = true;
    else if (arg === '--save-runtime-errors') parsed.saveRuntimeErrors = true;
    else if (arg === '--log') parsed.log = true;
    else if (arg === '--clear-issues') parsed.clearIssues = true;
    else if (arg === '--case-log') parsed.caseLog = parseCaseLogMode(argv[++index]);
    else if (arg.startsWith('--case-log=')) parsed.caseLog = parseCaseLogMode(arg.slice(11));
    else if (arg === '--error') {
      const next = argv[index + 1];
      if (next && !next.startsWith('--')) {
        parsed.errorLevel = parseErrorLevel(next);
        index += 1;
      } else {
        parsed.errorLevel = 5;
      }
    } else if (arg.startsWith('--error=')) parsed.errorLevel = parseErrorLevel(arg.slice(8));
    else if (arg === '--error-limit' || arg === '--max-errors') {
      parsed.errorLimit = parseErrorLimit(argv[++index]);
    } else if (arg.startsWith('--error-limit=')) parsed.errorLimit = parseErrorLimit(arg.slice(14));
    else if (arg.startsWith('--max-errors=')) parsed.errorLimit = parseErrorLimit(arg.slice(13));
    else if (arg === '--no-corpus-seeds') parsed.corpusSeeds = false;
    else if (arg === '--no-boundary-seeds') parsed.boundarySeeds = false;
    else if (arg === '--no-js-fuzzer') parsed.jsFuzzer = false;
    else if (arg === '--rebuild-js-fuzzer-db') parsed.rebuildJsFuzzerDb = true;
    else if (arg === '--count' || arg.startsWith('--count=')) {
      throw new Error('unknown option: --count has been replaced by --r, which means generated programs per corpus case');
    } else if (arg === '--r') parsed.r = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--r=')) parsed.r = Number.parseInt(arg.slice(4), 10);
    else if (arg === '--threads') parsed.threads = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--threads=')) parsed.threads = Number.parseInt(arg.slice(10), 10);
    else if (arg === '--time') parsed.timeMs = parseDuration(argv[++index], '--time');
    else if (arg.startsWith('--time=')) parsed.timeMs = parseDuration(arg.slice(7), '--time');
    else if (arg === '--seeds') parsed.seeds = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--seeds=')) parsed.seeds = Number.parseInt(arg.slice(8), 10);
    else if (arg === '--base-seed') parsed.baseSeed = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--base-seed=')) parsed.baseSeed = Number.parseInt(arg.slice(12), 10);
    else if (arg === '--host-timeout-ms') parsed.hostTimeoutMs = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--host-timeout-ms=')) parsed.hostTimeoutMs = Number.parseInt(arg.slice(18), 10);
    else if (arg === '--vm-timeout-ms') parsed.vmTimeoutMs = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--vm-timeout-ms=')) parsed.vmTimeoutMs = Number.parseInt(arg.slice(16), 10);
    else if (arg === '--differential') parsed.differential = true;
    else if (arg === '--no-differential') parsed.differential = false;
    else if (arg === '--reference-engine') parsed.referenceEngine = String(argv[++index] || '');
    else if (arg.startsWith('--reference-engine=')) parsed.referenceEngine = arg.slice(19);
    else if (arg === '--v8-path') parsed.referenceEnginePath = String(argv[++index] || '');
    else if (arg.startsWith('--v8-path=')) parsed.referenceEnginePath = arg.slice(10);
    else if (arg === '--differential-stderr') parsed.differentialStderr = String(argv[++index] || '');
    else if (arg.startsWith('--differential-stderr=')) parsed.differentialStderr = arg.slice(22);
    else if (arg === '--worker-idle-check-ms' || arg === '--worker-grace-ms') {
      parsed.workerIdleCheckMs = Number.parseInt(argv[++index], 10);
    } else if (arg.startsWith('--worker-idle-check-ms=')) {
      parsed.workerIdleCheckMs = Number.parseInt(arg.slice(23), 10);
    } else if (arg.startsWith('--worker-grace-ms=')) parsed.workerIdleCheckMs = Number.parseInt(arg.slice(18), 10);
    else if (arg === '--worker-stuck-ms') parsed.workerStuckMs = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--worker-stuck-ms=')) parsed.workerStuckMs = Number.parseInt(arg.slice(18), 10);
    else if (arg === '--max-bytes' || arg === '--max-generated-bytes') {
      parsed.maxBytes = Number.parseInt(argv[++index], 10);
    } else if (arg.startsWith('--max-bytes=')) parsed.maxBytes = Number.parseInt(arg.slice(12), 10);
    else if (arg.startsWith('--max-generated-bytes=')) {
      parsed.maxBytes = Number.parseInt(arg.slice(22), 10);
    } else if (arg === '--save-failures') parsed.saveFailures = path.resolve(argv[++index]);
    else if (arg.startsWith('--save-failures=')) parsed.saveFailures = path.resolve(arg.slice(16));
    else if (arg === '--issue-dir') parsed.issueDir = path.resolve(argv[++index]);
    else if (arg.startsWith('--issue-dir=')) parsed.issueDir = path.resolve(arg.slice(12));
    else if (arg === '--replay-issues') parsed.replayIssues = argv[++index];
    else if (arg.startsWith('--replay-issues=')) parsed.replayIssues = arg.slice(16);
    else if (arg === '--replay-timeout-ms') parsed.replayTimeoutMs = Number.parseInt(argv[++index], 10);
    else if (arg.startsWith('--replay-timeout-ms=')) parsed.replayTimeoutMs = Number.parseInt(arg.slice(20), 10);
    else if (arg === '--js-fuzzer-db') parsed.jsFuzzerDb = path.resolve(argv[++index]);
    else if (arg.startsWith('--js-fuzzer-db=')) parsed.jsFuzzerDb = path.resolve(arg.slice(15));
    else throw new Error(`unknown option: ${arg}`);
  }

  if (!Number.isFinite(parsed.r) || parsed.r < 1) {
    throw new Error('--r must be a positive integer');
  }
  if (!Number.isFinite(parsed.threads) || parsed.threads < 1) {
    throw new Error('--threads must be a positive integer');
  }
  if (!Number.isFinite(parsed.workerIdleCheckMs) || parsed.workerIdleCheckMs < 0) {
    throw new Error('--worker-idle-check-ms must be a non-negative integer');
  }
  if (!Number.isFinite(parsed.workerStuckMs) || parsed.workerStuckMs < 0) {
    throw new Error('--worker-stuck-ms must be a non-negative integer');
  }
  if (!Number.isFinite(parsed.replayTimeoutMs) || parsed.replayTimeoutMs < 1) {
    throw new Error('--replay-timeout-ms must be a positive integer');
  }
  if (parsed.vmTimeoutMs === null) parsed.vmTimeoutMs = parsed.hostTimeoutMs;
  if (!Number.isFinite(parsed.vmTimeoutMs) || parsed.vmTimeoutMs < 1) {
    throw new Error('--vm-timeout-ms must be a positive integer');
  }
  if (parsed.referenceEngine !== 'v8') {
    throw new Error('--reference-engine has been replaced by jsvu V8; use --reference-engine=v8 or --v8-path <file>');
  }
  if (!['summary', 'exact', 'ignore'].includes(parsed.differentialStderr)) {
    throw new Error('--differential-stderr must be summary, exact, or ignore');
  }
  parsed.caseLogMode = resolveCaseLogMode(parsed);
  parsed.errorLevel = parseErrorLevel(parsed.errorLevel);
  parsed.errorLimit = parseErrorLimit(parsed.errorLimit);

  return parsed;
}

function parseCaseLogMode(value) {
  const mode = String(value || '').trim();
  if (!['auto', 'all', 'failures', 'none'].includes(mode)) {
    throw new Error('--case-log must be one of auto, all, failures, or none');
  }
  return mode;
}

function resolveCaseLogMode(args) {
  if (args.threads > 1) return 'queued-issues';
  if (args.caseLog !== 'auto') return args.caseLog;
  return 'all';
}

function numberEnv(name, fallback) {
  return Number.parseInt(process.env[name] || String(fallback), 10);
}

function nullableNumberEnv(name) {
  const value = process.env[name];
  if (value === undefined || value === '') return null;
  return Number.parseInt(value, 10);
}

function stringEnv(name, fallback) {
  return process.env[name] || fallback;
}

function boolEnv(name, fallback) {
  const value = process.env[name];
  if (value === undefined || value === '') return fallback;
  return /^(?:1|true|yes|on)$/i.test(value);
}

function durationEnv(name, fallback) {
  const value = process.env[name];
  if (value === undefined || value === '') return fallback;
  return parseDuration(value, name);
}

function parseDuration(value, label) {
  if (value === undefined || value === null || value === '') {
    throw new Error(`${label} requires a duration`);
  }
  const match = String(value).trim().match(/^(\d+(?:\.\d+)?)(ms|s|m|h)?$/i);
  if (!match) {
    throw new Error(`${label} must be a positive duration like 500ms, 30s, 2m, or 1h`);
  }
  const amount = Number.parseFloat(match[1]);
  const unit = (match[2] || 's').toLowerCase();
  const multipliers = {
    ms: 1,
    s: 1000,
    m: 60_000,
    h: 3_600_000,
  };
  const ms = Math.ceil(amount * multipliers[unit]);
  if (!Number.isFinite(ms) || ms < 1) {
    throw new Error(`${label} must be greater than 0`);
  }
  return ms;
}

function parseErrorLevel(value) {
  const level = Number.parseInt(value, 10);
  if (!Number.isFinite(level) || level < 0 || level > 5) {
    throw new Error('--error must be an integer from 0 to 5');
  }
  return level;
}

function parseErrorLimit(value) {
  const text = String(value ?? '').trim();
  if (!/^\d+$/.test(text)) {
    throw new Error('--error-limit must be a non-negative integer');
  }
  return Number.parseInt(text, 10);
}

function mixSeed(...values) {
  let seed = 0x811c9dc5;
  for (const value of values) {
    seed ^= value >>> 0;
    seed = Math.imul(seed, 0x01000193) >>> 0;
  }
  return seed >>> 0;
}

function hash32(value) {
  const text = String(value);
  let hash = 0x811c9dc5;
  for (let index = 0; index < text.length; index += 1) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash >>> 0;
}

function isCompileError(error) {
  return /failed to parse|parse errors?|syntax|parser|compiler|unsupported|TS\d+/i.test(error);
}

function isInternalError(error) {
  return /RuntimeError|memory access out of bounds|unreachable|panic|assertion failed|invalid encoding seed|bad .* index|bad constant index|invalid .* operand/i.test(
    error,
  );
}

function isExpectedVmRuntimeError(error) {
  return /RangeError:\s+maximum execution steps exceeded/.test(error);
}

function isExpectedHostRuntimeError(source, args) {
  const v8Path = args.referenceEnginePath || resolveJsvuV8Path();
  if (!v8Path) return false;
  const result = spawnSync(v8Path, v8EvalArgs(v8ReferencePrelude() + source, v8Path), {
    encoding: 'utf8',
    timeout: args.hostTimeoutMs,
    maxBuffer: 1024 * 1024,
  });
  if (result.error || result.status === 0) return false;
  const output = `${result.stderr || ''}\n${result.stdout || ''}`;
  return /\b(?:TypeError|RangeError|ReferenceError|URIError|EvalError):/.test(output);
}

function errorText(err) {
  if (err && typeof err === 'object' && 'stack' in err) return String(err.stack);
  return String(err);
}

function firstLine(value) {
  return String(value).split(/\r?\n/, 1)[0];
}

function printHelp() {
  console.log(`Usage:
  node tests/Fuzz.js [options]

Options:
  --r <n>                    Generated JS programs per corpus case. Default: 4
  --threads <n>              Worker threads for parallel fuzz compile/run. Default: 1
  --case-log <mode>          Single-thread case result log mode: auto, all, failures, none.
                             Multi-thread fuzz uses queued issue logs and never prints every case.
  --time <duration>          Run as many fuzz cases as possible within a time budget.
                             Accepts 500ms, 30s, 2m, 1h. Bare numbers are seconds.
  --seeds <n>                Randomized encoding seeds per program. Default: 2
  --base-seed <n>            Deterministic generator seed. Default: 1337
  --host-timeout-ms <n>      jsvu V8 reference timeout for JS runtime errors. Default: 1000
  --vm-timeout-ms <n>        JS VM per-case timeout. Default: same as --host-timeout-ms
  --differential             Run reference engine first, save expected process result, and compare VM stdout/stderr/exitCode/signal/timeout.
  --reference-engine <name>  Compatibility option; only v8 is supported.
  --v8-path <file>           jsvu V8 binary path. Default: $JSVU_V8_PATH, $JSVU_V8, ~/.jsvu/bin/v8
  --differential-stderr <m>  Stderr comparison mode: summary, exact, ignore. Default: summary
  --worker-idle-check-ms <n> Check worker liveness after no messages for n ms while stopping. Default: 5000
  --worker-grace-ms <n>      Alias of --worker-idle-check-ms.
  --worker-stuck-ms <n>      Archive and terminate a worker if one case runs longer than n ms. 0=off. Default: 30000
  --max-bytes <n>            Skip generated programs larger than n bytes. Default: 8192
  --max-generated-bytes <n>  Alias of --max-bytes.
  --log                      Write full console output to tests/.issues/<YY-MM-DD:HH:mm:ss>/log.txt.
  --error <0-5>              Archive issue cases to tests/.issues/<YY-MM-DD:HH:mm:ss>. 0=off, 1=internal, 2=+compile,
                             3=+runtime, 4=+expected JS runtime, 5=+skipped. Bare --error means 5.
  --clear-issues             Clear all historical issue archives under --issue-dir before this run.
  --error-limit <n>          Stop fuzzing once compile/runtime/expected-runtime/internal errors reach n. 0=off.
  --max-errors <n>           Alias of --error-limit.
  --issue-dir <dir>          Issue archive root directory. Default: tests/.issues.
  --replay-issues <time|dir> Replay archived issue sources from tests/.issues/<time> or a directory.
  --replay-timeout-ms <n>    Timeout for each replay case. Default: 30000
  --save-failures <dir>      Save failing sources. Default: artifacts/js_fuzzer/failures
  --save-compile-errors      Also save parser/compiler failures.
  --save-runtime-errors      Also save JS runtime failures.
  --strict-runtime           Treat runtime JS errors as process failures.
  --fail-on-compile-error    Treat parser/compiler errors as process failures.
  --no-corpus-seeds          Do not derive fuzz seeds from tests/corpus.
  --no-boundary-seeds        Disable boundary value seed mixing.
  --no-js-fuzzer             Disable V8 js_fuzzer and use built-in templates only.
  --js-fuzzer-db <dir>       Mutation DB path. Default: tests/db
  --rebuild-js-fuzzer-db     Rebuild mutation DB from tests/corpus before fuzzing.
  --verbose                  Print non-fatal compile/runtime errors.

The fuzzer generates JS in memory and runs Source -> IR -> Bytecode -> bytes -> seed -> Runtime.
It uses V8 js_fuzzer when tests/.vendor/js_fuzzer is available, derives seeds from tests/corpus,
records observable VM output hashes, and reports opcode coverage feedback.
With --differential, each generated source is executed by the JS VM first, then by jsvu V8.
VM timeout + V8 timeout becomes TIMEOUT and is not compared; VM timeout/error + V8 success is
archived as a highest-level VM failure. Archived mismatches include .expected.json and
.actual.json next to the source for replay/debugging.
By default only internal VM failures are persisted to artifacts/js_fuzzer/failures. Use --error to
archive categorized compile/runtime/internal/skipped issues under tests/.issues/<YY-MM-DD:HH:mm:ss>.
Use --clear-issues to reset old issue archives, and --error-limit to stop early when errors pile up.`);
}
