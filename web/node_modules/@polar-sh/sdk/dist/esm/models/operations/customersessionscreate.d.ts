import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CustomerSessionCustomerExternalIDCreate, CustomerSessionCustomerExternalIDCreate$Outbound } from "../components/customersessioncustomerexternalidcreate.js";
import { CustomerSessionCustomerIDCreate, CustomerSessionCustomerIDCreate$Outbound } from "../components/customersessioncustomeridcreate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerSessionsCreateCustomerSessionCreate = CustomerSessionCustomerIDCreate | CustomerSessionCustomerExternalIDCreate;
/** @internal */
export declare const CustomerSessionsCreateCustomerSessionCreate$inboundSchema: z.ZodType<CustomerSessionsCreateCustomerSessionCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerSessionsCreateCustomerSessionCreate$Outbound = CustomerSessionCustomerIDCreate$Outbound | CustomerSessionCustomerExternalIDCreate$Outbound;
/** @internal */
export declare const CustomerSessionsCreateCustomerSessionCreate$outboundSchema: z.ZodType<CustomerSessionsCreateCustomerSessionCreate$Outbound, z.ZodTypeDef, CustomerSessionsCreateCustomerSessionCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSessionsCreateCustomerSessionCreate$ {
    /** @deprecated use `CustomerSessionsCreateCustomerSessionCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerSessionsCreateCustomerSessionCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerSessionsCreateCustomerSessionCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerSessionsCreateCustomerSessionCreate$Outbound, z.ZodTypeDef, CustomerSessionsCreateCustomerSessionCreate>;
    /** @deprecated use `CustomerSessionsCreateCustomerSessionCreate$Outbound` instead. */
    type Outbound = CustomerSessionsCreateCustomerSessionCreate$Outbound;
}
export declare function customerSessionsCreateCustomerSessionCreateToJSON(customerSessionsCreateCustomerSessionCreate: CustomerSessionsCreateCustomerSessionCreate): string;
export declare function customerSessionsCreateCustomerSessionCreateFromJSON(jsonString: string): SafeParseResult<CustomerSessionsCreateCustomerSessionCreate, SDKValidationError>;
//# sourceMappingURL=customersessionscreate.d.ts.map