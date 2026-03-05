import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const RefundReason: {
    readonly Duplicate: "duplicate";
    readonly Fraudulent: "fraudulent";
    readonly CustomerRequest: "customer_request";
    readonly ServiceDisruption: "service_disruption";
    readonly SatisfactionGuarantee: "satisfaction_guarantee";
    readonly Other: "other";
};
export type RefundReason = ClosedEnum<typeof RefundReason>;
/** @internal */
export declare const RefundReason$inboundSchema: z.ZodNativeEnum<typeof RefundReason>;
/** @internal */
export declare const RefundReason$outboundSchema: z.ZodNativeEnum<typeof RefundReason>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace RefundReason$ {
    /** @deprecated use `RefundReason$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly Duplicate: "duplicate";
        readonly Fraudulent: "fraudulent";
        readonly CustomerRequest: "customer_request";
        readonly ServiceDisruption: "service_disruption";
        readonly SatisfactionGuarantee: "satisfaction_guarantee";
        readonly Other: "other";
    }>;
    /** @deprecated use `RefundReason$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly Duplicate: "duplicate";
        readonly Fraudulent: "fraudulent";
        readonly CustomerRequest: "customer_request";
        readonly ServiceDisruption: "service_disruption";
        readonly SatisfactionGuarantee: "satisfaction_guarantee";
        readonly Other: "other";
    }>;
}
//# sourceMappingURL=refundreason.d.ts.map