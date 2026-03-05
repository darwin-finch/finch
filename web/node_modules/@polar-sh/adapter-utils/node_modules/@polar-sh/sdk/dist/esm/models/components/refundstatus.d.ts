import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const RefundStatus: {
    readonly Pending: "pending";
    readonly Succeeded: "succeeded";
    readonly Failed: "failed";
    readonly Canceled: "canceled";
};
export type RefundStatus = ClosedEnum<typeof RefundStatus>;
/** @internal */
export declare const RefundStatus$inboundSchema: z.ZodNativeEnum<typeof RefundStatus>;
/** @internal */
export declare const RefundStatus$outboundSchema: z.ZodNativeEnum<typeof RefundStatus>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundStatus$ {
    /** @deprecated use `RefundStatus$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly Pending: "pending";
        readonly Succeeded: "succeeded";
        readonly Failed: "failed";
        readonly Canceled: "canceled";
    }>;
    /** @deprecated use `RefundStatus$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly Pending: "pending";
        readonly Succeeded: "succeeded";
        readonly Failed: "failed";
        readonly Canceled: "canceled";
    }>;
}
//# sourceMappingURL=refundstatus.d.ts.map