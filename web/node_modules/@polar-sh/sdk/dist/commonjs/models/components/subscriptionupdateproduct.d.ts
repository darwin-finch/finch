import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { SubscriptionProrationBehavior } from "./subscriptionprorationbehavior.js";
export type SubscriptionUpdateProduct = {
    /**
     * Update subscription to another product.
     */
    productId: string;
    /**
     * Determine how to handle the proration billing. If not provided, will use the default organization setting.
     */
    prorationBehavior?: SubscriptionProrationBehavior | null | undefined;
};
/** @internal */
export declare const SubscriptionUpdateProduct$inboundSchema: z.ZodType<SubscriptionUpdateProduct, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionUpdateProduct$Outbound = {
    product_id: string;
    proration_behavior?: string | null | undefined;
};
/** @internal */
export declare const SubscriptionUpdateProduct$outboundSchema: z.ZodType<SubscriptionUpdateProduct$Outbound, z.ZodTypeDef, SubscriptionUpdateProduct>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionUpdateProduct$ {
    /** @deprecated use `SubscriptionUpdateProduct$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionUpdateProduct, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionUpdateProduct$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionUpdateProduct$Outbound, z.ZodTypeDef, SubscriptionUpdateProduct>;
    /** @deprecated use `SubscriptionUpdateProduct$Outbound` instead. */
    type Outbound = SubscriptionUpdateProduct$Outbound;
}
export declare function subscriptionUpdateProductToJSON(subscriptionUpdateProduct: SubscriptionUpdateProduct): string;
export declare function subscriptionUpdateProductFromJSON(jsonString: string): SafeParseResult<SubscriptionUpdateProduct, SDKValidationError>;
//# sourceMappingURL=subscriptionupdateproduct.d.ts.map