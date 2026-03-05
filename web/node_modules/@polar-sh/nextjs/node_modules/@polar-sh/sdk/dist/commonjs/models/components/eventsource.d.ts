import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const EventSource: {
    readonly System: "system";
    readonly User: "user";
};
export type EventSource = ClosedEnum<typeof EventSource>;
/** @internal */
export declare const EventSource$inboundSchema: z.ZodNativeEnum<typeof EventSource>;
/** @internal */
export declare const EventSource$outboundSchema: z.ZodNativeEnum<typeof EventSource>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventSource$ {
    /** @deprecated use `EventSource$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly System: "system";
        readonly User: "user";
    }>;
    /** @deprecated use `EventSource$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly System: "system";
        readonly User: "user";
    }>;
}
//# sourceMappingURL=eventsource.d.ts.map