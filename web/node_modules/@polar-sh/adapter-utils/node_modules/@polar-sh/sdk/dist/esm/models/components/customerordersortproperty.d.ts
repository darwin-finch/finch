import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
export declare const CustomerOrderSortProperty: {
    readonly CreatedAt: "created_at";
    readonly MinusCreatedAt: "-created_at";
    readonly Amount: "amount";
    readonly MinusAmount: "-amount";
    readonly NetAmount: "net_amount";
    readonly MinusNetAmount: "-net_amount";
    readonly Product: "product";
    readonly MinusProduct: "-product";
    readonly Subscription: "subscription";
    readonly MinusSubscription: "-subscription";
};
export type CustomerOrderSortProperty = ClosedEnum<typeof CustomerOrderSortProperty>;
/** @internal */
export declare const CustomerOrderSortProperty$inboundSchema: z.ZodNativeEnum<typeof CustomerOrderSortProperty>;
/** @internal */
export declare const CustomerOrderSortProperty$outboundSchema: z.ZodNativeEnum<typeof CustomerOrderSortProperty>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerOrderSortProperty$ {
    /** @deprecated use `CustomerOrderSortProperty$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly CreatedAt: "created_at";
        readonly MinusCreatedAt: "-created_at";
        readonly Amount: "amount";
        readonly MinusAmount: "-amount";
        readonly NetAmount: "net_amount";
        readonly MinusNetAmount: "-net_amount";
        readonly Product: "product";
        readonly MinusProduct: "-product";
        readonly Subscription: "subscription";
        readonly MinusSubscription: "-subscription";
    }>;
    /** @deprecated use `CustomerOrderSortProperty$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly CreatedAt: "created_at";
        readonly MinusCreatedAt: "-created_at";
        readonly Amount: "amount";
        readonly MinusAmount: "-amount";
        readonly NetAmount: "net_amount";
        readonly MinusNetAmount: "-net_amount";
        readonly Product: "product";
        readonly MinusProduct: "-product";
        readonly Subscription: "subscription";
        readonly MinusSubscription: "-subscription";
    }>;
}
//# sourceMappingURL=customerordersortproperty.d.ts.map