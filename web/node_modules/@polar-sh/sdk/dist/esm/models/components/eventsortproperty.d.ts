import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const EventSortProperty: {
    readonly Timestamp: "timestamp";
    readonly MinusTimestamp: "-timestamp";
};
export type EventSortProperty = ClosedEnum<typeof EventSortProperty>;
/** @internal */
export declare const EventSortProperty$inboundSchema: z.ZodNativeEnum<typeof EventSortProperty>;
/** @internal */
export declare const EventSortProperty$outboundSchema: z.ZodNativeEnum<typeof EventSortProperty>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventSortProperty$ {
    /** @deprecated use `EventSortProperty$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly Timestamp: "timestamp";
        readonly MinusTimestamp: "-timestamp";
    }>;
    /** @deprecated use `EventSortProperty$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly Timestamp: "timestamp";
        readonly MinusTimestamp: "-timestamp";
    }>;
}
//# sourceMappingURL=eventsortproperty.d.ts.map