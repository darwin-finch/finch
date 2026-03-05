import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const TimeInterval: {
    readonly Year: "year";
    readonly Month: "month";
    readonly Week: "week";
    readonly Day: "day";
    readonly Hour: "hour";
};
export type TimeInterval = ClosedEnum<typeof TimeInterval>;
/** @internal */
export declare const TimeInterval$inboundSchema: z.ZodNativeEnum<typeof TimeInterval>;
/** @internal */
export declare const TimeInterval$outboundSchema: z.ZodNativeEnum<typeof TimeInterval>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace TimeInterval$ {
    /** @deprecated use `TimeInterval$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly Year: "year";
        readonly Month: "month";
        readonly Week: "week";
        readonly Day: "day";
        readonly Hour: "hour";
    }>;
    /** @deprecated use `TimeInterval$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly Year: "year";
        readonly Month: "month";
        readonly Week: "week";
        readonly Day: "day";
        readonly Hour: "hour";
    }>;
}
//# sourceMappingURL=timeinterval.d.ts.map