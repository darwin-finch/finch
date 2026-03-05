import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Schema for creating a customer session using a customer ID.
 */
export type CustomerSessionCustomerIDCreate = {
    /**
     * ID of the customer to create a session for.
     */
    customerId: string;
};
/** @internal */
export declare const CustomerSessionCustomerIDCreate$inboundSchema: z.ZodType<CustomerSessionCustomerIDCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerSessionCustomerIDCreate$Outbound = {
    customer_id: string;
};
/** @internal */
export declare const CustomerSessionCustomerIDCreate$outboundSchema: z.ZodType<CustomerSessionCustomerIDCreate$Outbound, z.ZodTypeDef, CustomerSessionCustomerIDCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSessionCustomerIDCreate$ {
    /** @deprecated use `CustomerSessionCustomerIDCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerSessionCustomerIDCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerSessionCustomerIDCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerSessionCustomerIDCreate$Outbound, z.ZodTypeDef, CustomerSessionCustomerIDCreate>;
    /** @deprecated use `CustomerSessionCustomerIDCreate$Outbound` instead. */
    type Outbound = CustomerSessionCustomerIDCreate$Outbound;
}
export declare function customerSessionCustomerIDCreateToJSON(customerSessionCustomerIDCreate: CustomerSessionCustomerIDCreate): string;
export declare function customerSessionCustomerIDCreateFromJSON(jsonString: string): SafeParseResult<CustomerSessionCustomerIDCreate, SDKValidationError>;
//# sourceMappingURL=customersessioncustomeridcreate.d.ts.map