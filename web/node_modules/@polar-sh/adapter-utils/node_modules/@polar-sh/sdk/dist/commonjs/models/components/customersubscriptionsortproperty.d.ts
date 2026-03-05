import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const CustomerSubscriptionSortProperty: {
    readonly StartedAt: "started_at";
    readonly MinusStartedAt: "-started_at";
    readonly Amount: "amount";
    readonly MinusAmount: "-amount";
    readonly Status: "status";
    readonly MinusStatus: "-status";
    readonly Organization: "organization";
    readonly MinusOrganization: "-organization";
    readonly Product: "product";
    readonly MinusProduct: "-product";
};
export type CustomerSubscriptionSortProperty = ClosedEnum<typeof CustomerSubscriptionSortProperty>;
/** @internal */
export declare const CustomerSubscriptionSortProperty$inboundSchema: z.ZodNativeEnum<typeof CustomerSubscriptionSortProperty>;
/** @internal */
export declare const CustomerSubscriptionSortProperty$outboundSchema: z.ZodNativeEnum<typeof CustomerSubscriptionSortProperty>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSubscriptionSortProperty$ {
    /** @deprecated use `CustomerSubscriptionSortProperty$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly StartedAt: "started_at";
        readonly MinusStartedAt: "-started_at";
        readonly Amount: "amount";
        readonly MinusAmount: "-amount";
        readonly Status: "status";
        readonly MinusStatus: "-status";
        readonly Organization: "organization";
        readonly MinusOrganization: "-organization";
        readonly Product: "product";
        readonly MinusProduct: "-product";
    }>;
    /** @deprecated use `CustomerSubscriptionSortProperty$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly StartedAt: "started_at";
        readonly MinusStartedAt: "-started_at";
        readonly Amount: "amount";
        readonly MinusAmount: "-amount";
        readonly Status: "status";
        readonly MinusStatus: "-status";
        readonly Organization: "organization";
        readonly MinusOrganization: "-organization";
        readonly Product: "product";
        readonly MinusProduct: "-product";
    }>;
}
//# sourceMappingURL=customersubscriptionsortproperty.d.ts.map