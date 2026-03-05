import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerSubscriptionUpdateProduct = {
    /**
     * Update subscription to another product.
     */
    productId: string;
};
/** @internal */
export declare const CustomerSubscriptionUpdateProduct$inboundSchema: z.ZodType<CustomerSubscriptionUpdateProduct, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerSubscriptionUpdateProduct$Outbound = {
    product_id: string;
};
/** @internal */
export declare const CustomerSubscriptionUpdateProduct$outboundSchema: z.ZodType<CustomerSubscriptionUpdateProduct$Outbound, z.ZodTypeDef, CustomerSubscriptionUpdateProduct>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSubscriptionUpdateProduct$ {
    /** @deprecated use `CustomerSubscriptionUpdateProduct$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerSubscriptionUpdateProduct, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerSubscriptionUpdateProduct$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerSubscriptionUpdateProduct$Outbound, z.ZodTypeDef, CustomerSubscriptionUpdateProduct>;
    /** @deprecated use `CustomerSubscriptionUpdateProduct$Outbound` instead. */
    type Outbound = CustomerSubscriptionUpdateProduct$Outbound;
}
export declare function customerSubscriptionUpdateProductToJSON(customerSubscriptionUpdateProduct: CustomerSubscriptionUpdateProduct): string;
export declare function customerSubscriptionUpdateProductFromJSON(jsonString: string): SafeParseResult<CustomerSubscriptionUpdateProduct, SDKValidationError>;
//# sourceMappingURL=customersubscriptionupdateproduct.d.ts.map