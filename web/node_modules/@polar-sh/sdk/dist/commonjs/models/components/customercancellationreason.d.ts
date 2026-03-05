import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const CustomerCancellationReason: {
    readonly CustomerService: "customer_service";
    readonly LowQuality: "low_quality";
    readonly MissingFeatures: "missing_features";
    readonly SwitchedService: "switched_service";
    readonly TooComplex: "too_complex";
    readonly TooExpensive: "too_expensive";
    readonly Unused: "unused";
    readonly Other: "other";
};
export type CustomerCancellationReason = ClosedEnum<typeof CustomerCancellationReason>;
/** @internal */
export declare const CustomerCancellationReason$inboundSchema: z.ZodNativeEnum<typeof CustomerCancellationReason>;
/** @internal */
export declare const CustomerCancellationReason$outboundSchema: z.ZodNativeEnum<typeof CustomerCancellationReason>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerCancellationReason$ {
    /** @deprecated use `CustomerCancellationReason$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly CustomerService: "customer_service";
        readonly LowQuality: "low_quality";
        readonly MissingFeatures: "missing_features";
        readonly SwitchedService: "switched_service";
        readonly TooComplex: "too_complex";
        readonly TooExpensive: "too_expensive";
        readonly Unused: "unused";
        readonly Other: "other";
    }>;
    /** @deprecated use `CustomerCancellationReason$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly CustomerService: "customer_service";
        readonly LowQuality: "low_quality";
        readonly MissingFeatures: "missing_features";
        readonly SwitchedService: "switched_service";
        readonly TooComplex: "too_complex";
        readonly TooExpensive: "too_expensive";
        readonly Unused: "unused";
        readonly Other: "other";
    }>;
}
//# sourceMappingURL=customercancellationreason.d.ts.map