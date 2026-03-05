import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Schema for creating a customer session using an external customer ID.
 */
export type CustomerSessionCustomerExternalIDCreate = {
    /**
     * External ID of the customer to create a session for.
     */
    customerExternalId: string;
};
/** @internal */
export declare const CustomerSessionCustomerExternalIDCreate$inboundSchema: z.ZodType<CustomerSessionCustomerExternalIDCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerSessionCustomerExternalIDCreate$Outbound = {
    customer_external_id: string;
};
/** @internal */
export declare const CustomerSessionCustomerExternalIDCreate$outboundSchema: z.ZodType<CustomerSessionCustomerExternalIDCreate$Outbound, z.ZodTypeDef, CustomerSessionCustomerExternalIDCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSessionCustomerExternalIDCreate$ {
    /** @deprecated use `CustomerSessionCustomerExternalIDCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerSessionCustomerExternalIDCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerSessionCustomerExternalIDCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerSessionCustomerExternalIDCreate$Outbound, z.ZodTypeDef, CustomerSessionCustomerExternalIDCreate>;
    /** @deprecated use `CustomerSessionCustomerExternalIDCreate$Outbound` instead. */
    type Outbound = CustomerSessionCustomerExternalIDCreate$Outbound;
}
export declare function customerSessionCustomerExternalIDCreateToJSON(customerSessionCustomerExternalIDCreate: CustomerSessionCustomerExternalIDCreate): string;
export declare function customerSessionCustomerExternalIDCreateFromJSON(jsonString: string): SafeParseResult<CustomerSessionCustomerExternalIDCreate, SDKValidationError>;
//# sourceMappingURL=customersessioncustomerexternalidcreate.d.ts.map