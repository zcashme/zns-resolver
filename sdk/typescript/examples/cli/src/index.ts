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
  listings                     Show names for sale
  status                       Show resolver status
  events [name]                Show recent events
  cost <name>                  Show claim cost

Options:
  --url <url>                  Resolver URL
  --help                       Show this help

Environment:
  ZNS_URL                      Resolver URL

Examples:
  zns resolve alice
  zns available bob
  zns status
  zns events alice`);
}

async function main() {
  const { flags, positional } = parseArgs(args);
  const url = flags.url || process.env.ZNS_URL;
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
    case "listings": {
      const result = await zns.listings();
      if (result.listings.length === 0) {
        console.log("No listings");
        break;
      }
      for (const listing of result.listings) {
        console.log(`${listing.name} – ${listing.price / 1e8} ZEC`);
      }
      break;
    }
    case "status": {
      const status = await zns.status();
      console.log(`Synced:     ${status.syncedHeight}`);
      console.log(`UIVK:       ${status.uivk.slice(0, 30)}...`);
      console.log(`Registry:   ${status.address}`);
      console.log(`Registered: ${status.registered}`);
      console.log(`Listed:     ${status.listed}`);
      if (status.pricing) {
        console.log(`Pricing:    ${status.pricing.tiers.map((tier, i) => `${i + 1}ch=${tier / 1e8}ZEC`).join(" ")}`);
      }
      break;
    }
    case "events": {
      const result = await zns.events(positional[0] ? { name: positional[0] } : {});
      for (const event of result.events) {
        const parts = [event.action, event.name];
        if (event.price !== null) parts.push(`${event.price / 1e8} ZEC`);
        parts.push(`h=${event.height}`);
        console.log(parts.join("  "));
      }
      console.log(`(${result.total} total)`);
      break;
    }
    case "cost": {
      const name = positional[0];
      if (!name) throw new Error("Usage: zns cost <name>");
      if (!zns.isValidName(zns.normalizeName(name))) {
        console.log("Invalid name");
        break;
      }
      const status = await zns.status();
      const cost = status.pricing && zns.claimCost(name.length, status.pricing);
      if (cost == null) console.log("No pricing available");
      else console.log(`${name}: ${cost / 1e8} ZEC (${cost} zats)`);
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
