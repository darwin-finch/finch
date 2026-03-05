import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { SDKOptions } from "../lib/config.js";
import type { ConsoleLogger } from "./console-logger.js";
import { MCPScope } from "./scopes.js";
export declare function createMCPServer(deps: {
    logger: ConsoleLogger;
    allowedTools?: string[] | undefined;
    scopes?: MCPScope[] | undefined;
    serverURL?: string | undefined;
    accessToken?: SDKOptions["accessToken"] | undefined;
    server?: SDKOptions["server"] | undefined;
}): McpServer;
//# sourceMappingURL=server.d.ts.map