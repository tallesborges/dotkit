import { homedir } from "node:os";
import { join } from "node:path";
import {
  createTerminalAdapter,
  renderQrCode,
  waitForSessions,
} from "@parity/product-sdk-terminal";
import { AccountId } from "polkadot-api";

process.umask(0o077);

function argumentsFromProcess() {
  const args = process.argv.slice(2);
  if (args.length !== 3) {
    throw new Error("usage: login.mjs <app-id> <metadata-url> <people-rpc>");
  }

  const [appId, metadataUrl, peopleRpc] = args;
  if (!appId.trim()) {
    throw new Error("app id must not be empty");
  }

  let metadata;
  try {
    metadata = new URL(metadataUrl);
  } catch {
    throw new Error("metadata URL must be a valid HTTPS URL");
  }
  if (metadata.protocol !== "https:") {
    throw new Error("metadata URL must use HTTPS");
  }
  if (!peopleRpc.trim()) {
    throw new Error("People RPC endpoint must not be empty");
  }

  return { appId, metadataUrl, peopleRpc };
}

function accountAddress(session) {
  return AccountId().dec(new Uint8Array(session.remoteAccount.accountId));
}

async function main() {
  const { appId, metadataUrl, peopleRpc } = argumentsFromProcess();
  const storageDir = join(homedir(), ".dotkit", "papp", "sessions");
  const adapter = createTerminalAdapter({
    appId,
    metadataUrl,
    storageDir,
    endpoints: [peopleRpc],
    hostMetadata: { osType: "dotkit CLI" },
  });

  let pairingError;
  let interrupted = false;
  const stopStatus = adapter.sso.pairingStatus.subscribe((status) => {
    if (status.step === "pairing") {
      void renderQrCode(status.payload)
        .then((qr) => {
          console.log(qr);
          console.log(`\n${status.payload}`);
          console.log("Scan this code with your Polkadot mobile wallet to pair dotkit.");
        })
        .catch((error) => {
          pairingError = new Error(`could not render pairing QR: ${error.message}`);
          adapter.sso.abortAuthentication();
        });
    }
    if (status.step === "pairingError") {
      pairingError = new Error(`pairing failed: ${status.message}`);
      adapter.sso.abortAuthentication();
    }
  });
  const interrupt = () => {
    interrupted = true;
    adapter.sso.abortAuthentication();
  };
  process.once("SIGINT", interrupt);

  try {
    const existing = await waitForSessions(adapter, 1_000);
    if (existing.length > 0) {
      console.log(`Paired account: ${accountAddress(existing[0])}`);
      return;
    }

    const result = await adapter.sso.authenticate().match(
      (session) => ({ session }),
      (error) => ({ error }),
    );
    if (pairingError) {
      throw pairingError;
    }
    if (interrupted) {
      throw new Error("pairing cancelled");
    }
    if ("error" in result) {
      throw new Error(`pairing failed: ${result.error.message}`);
    }

    const sessions = await waitForSessions(adapter, 1_000);
    if (sessions.length === 0) {
      throw new Error("pairing completed but no session was saved");
    }
    console.log(`Paired account: ${accountAddress(sessions[0])}`);
  } finally {
    process.off("SIGINT", interrupt);
    stopStatus();
    await adapter.destroy();
  }
}

main().catch((error) => {
  console.error(`dotkit account login: ${error.message}`);
  process.exitCode = 1;
});