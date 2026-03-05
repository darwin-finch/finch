import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CustomerSubscriptionCancel, CustomerSubscriptionCancel$Outbound } from "./customersubscriptioncancel.js";
import { CustomerSubscriptionUpdateProduct, CustomerSubscriptionUpdateProduct$Outbound } from "./customersubscriptionupdateproduct.js";
export type CustomerSubscriptionUpdate = CustomerSubscriptionUpdateProduct | CustomerSubscriptionCancel;
/** @internal */
export declare const CustomerSubscriptionUpdate$inboundSchema: z.ZodType<CustomerSubscriptionUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerSubscriptionUpdate$Outbound = CustomerSubscriptionUpdateProduct$Outbound | CustomerSubscriptionCancel$Outbound;
/** @internal */
export declare const CustomerSubscriptionUpdate$outboundSchema: z.ZodType<CustomerSubscriptionUpdate$Outbound, z.ZodTypeDef, CustomerSubscriptionUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSubscriptionUpdate$ {
    /** @deprecated use `CustomerSubscriptionUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerSubscriptionUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerSubscriptionUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerSubscriptionUpdate$Outbound, z.ZodTypeDef, CustomerSubscriptionUpdate>;
    /** @deprecated use `CustomerSubscriptionUpdate$Outbound` instead. */
    type Outbound = CustomerSubscriptionUpdate$Outbound;
}
export declare function customerSubscriptionUpdateToJSON(customerSubscriptionUpdate: CustomerSubscriptionUpdate): string;
export declare function customerSubscriptionUpdateFromJSON(jsonString: string): SafeParseResult<CustomerSubscriptionUpdate, SDKValidationError>;
//# sourceMappingURL=customersubscriptionupdate.d.ts.map