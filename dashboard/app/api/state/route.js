import { readChainState } from "../../../lib/chain.js";
import { readResponse } from "../../../lib/api-response.js";

// Always read fresh on-chain state, never cache.
export const dynamic = "force-dynamic";
export const revalidate = 0;

export async function GET() {
  return readResponse(readChainState, "state_unavailable");
}
