import { runWorker, buildWorkerIo } from "./helper.mjs";

process.on("unhandledRejection", (reason) => {
  const message = `unhandledRejection in bridge worker: ${reason instanceof Error ? reason.stack : String(reason)}\n`;
  process.stderr.write(message, () => process.exit(1));
});

(async () => {
  const [verb, ...args] = process.argv.slice(2);
  process.exitCode = await runWorker(verb, args, buildWorkerIo());
})();
