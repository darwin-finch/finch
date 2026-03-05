import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const SubscriptionProrationBehavior: {
    readonly Invoice: "invoice";
    readonly Prorate: "prorate";
};
export type SubscriptionProrationBehavior = ClosedEnum<typeof SubscriptionProrationBehavior>;
/** @internal */
export declare const SubscriptionProrationBehavior$inboundSchema: z.ZodNativeEnum<typeof SubscriptionProrationBehavior>;
/** @internal */
export declare const SubscriptionProrationBehavior$outboundSchema: z.ZodNativeEnum<typeof SubscriptionProrationBehavior>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionProrationBehavior$ {
    /** @deprecated use `SubscriptionProrationBehavior$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly Invoice: "invoice";
        readonly Prorate: "prorate";
    }>;
    /** @deprecated use `SubscriptionProrationBehavior$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly Invoice: "invoice";
        readonly Prorate: "prorate";
    }>;
}
//# sourceMappingURL=subscriptionprorationbehavior.d.ts.map