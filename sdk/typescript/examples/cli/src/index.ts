import { ZNS } from "zcashname-sdk";

const command = process.argv[2];
const args = process.argv.slice(3);

function parseArgs(values: string[]): { flags: Record<string, string>; positional: string[] } {
  const flags: Record<string, string> = {};
  const positional: string[] = [];
  for (let i = 0; i < values.length; i++) {
    if (values[i].startsWith("--")) {
      const key = values[i].slice(2);
      const value = values[i + 1] ?? "";
      if (!value.startsWith("--")) {
        flags[key] = value;
        i++;
      } else {
        flags[key] = "true";
      }
    } else {
      positional.push(values[i]);
    }
  }
  return { flags, positional };
}

function help() {
  console.log(`zns <command> [args] [options]

Commands:
  resolve <name|address>       Resolve a name or address
  available <name>             Check name availability
  status                       Show resolver status
  events [name]                Show recent events

Options:
  --url <url>                  Resolver URL
  --help                       Show this help

Environment:
  ZNS_URL                      Resolver URL (required unless --url is set)

Examples:
  zns resolve alice
  zns available bob
  zns status
  zns events alice`);
}

async function main() {
  const { flags, positional } = parseArgs(args);
  const url = flags.url || process.env.ZNS_URL;
  if (!url) throw new Error("Set ZNS_URL or pass --url <resolver-url>");
  const zns = new ZNS({ url });

  switch (command) {
    case "resolve": {
      const query = positional[0];
      if (!query) throw new Error("Usage: zns resolve <name-or-address>");
      const result = query.startsWith("u")
        ? await zns.resolveAddress(query)
        : await zns.resolveName(query);
      const records = Array.isArray(result) ? result : result ? [result] : [];
      if (records.length === 0) {
        console.log("Not found");
        break;
      }
      for (const record of records) {
        console.log(`${record.name} → ${record.address}`);
        console.log(`  txid: ${record.txid}  height: ${record.height}`);
        console.log(`  last_action: ${record.lastAction}`);
      }
      break;
    }
    case "available": {
      const name = positional[0];
      if (!name) throw new Error("Usage: zns available <name>");
      if (!zns.isValidName(zns.normalizeName(name))) {
        console.log("Invalid name (use lowercase letters and digits)");
        break;
      }
      console.log((await zns.isAvailable(name)) ? "Available" : "Taken");
      break;
    }
    case "status": {
      const status = await zns.status();
      console.log(`Height:     ${status.syncedHeight}`);
      console.log(`Synced:     ${status.synced}`);
      console.log(`Viewing key: ${status.viewingKey.slice(0, 30)}...`);
      console.log(`Registered: ${status.registered}`);
      break;
    }
    case "events": {
      const result = await zns.events(positional[0] ? { name: positional[0] } : {});
      for (const event of result.events) {
        const parts = [event.action, event.name, event.address];
        parts.push(`h=${event.height}`, `action=${event.actionIndex}`);
        parts.push(`expires=${event.expiresAt}`);
        console.log(parts.join("  "));
      }
      console.log(`(${result.total} total)`);
      break;
    }
    default:
      help();
  }
}

main().catch((error: unknown) => {
  console.error("Error:", error instanceof Error ? error.message : error);
  process.exit(1);
});
