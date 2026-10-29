import { readOracleState } from "../../../lib/chain.js";
import { readResponse } from "../../../lib/api-response.js";

// Oracle reference checks only; this is not a trade authorization endpoint.
export const dynamic = "force-dynamic";
export const revalidate = 0;

export async function GET() {
  return readResponse(readOracleState, "oracle_unavailable");
}
