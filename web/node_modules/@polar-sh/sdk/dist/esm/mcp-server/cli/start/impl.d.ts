import { SDKOptions } from "../../../lib/config.js";
import { LocalContext } from "../../cli.js";
import { ConsoleLoggerLevel } from "../../console-logger.js";
import { MCPScope } from "../../scopes.js";
interface StartCommandFlags {
    readonly transport: "stdio" | "sse";
    readonly port: number;
    readonly tool?: string[];
    readonly scope?: MCPScope[];
    readonly "access-token"?: string | undefined;
    readonly "server-url"?: string;
    readonly server?: SDKOptions["server"];
    readonly "log-level": ConsoleLoggerLevel;
    readonly env?: [string, string][];
}
export declare function main(this: LocalContext, flags: StartCommandFlags): Promise<void>;
export {};
//# sourceMappingURL=impl.d.ts.map