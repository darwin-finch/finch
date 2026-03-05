import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Customer, Customer$Outbound } from "./customer.js";
/**
 * A customer session that can be used to authenticate as a customer.
 */
export type CustomerSession = {
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the object.
     */
    id: string;
    token: string;
    expiresAt: Date;
    customerPortalUrl: string;
    customerId: string;
    /**
     * A customer in an organization.
     */
    customer: Customer;
};
/** @internal */
export declare const CustomerSession$inboundSchema: z.ZodType<CustomerSession, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerSession$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    token: string;
    expires_at: string;
    customer_portal_url: string;
    customer_id: string;
    customer: Customer$Outbound;
};
/** @internal */
export declare const CustomerSession$outboundSchema: z.ZodType<CustomerSession$Outbound, z.ZodTypeDef, CustomerSession>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSession$ {
    /** @deprecated use `CustomerSession$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerSession, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerSession$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerSession$Outbound, z.ZodTypeDef, CustomerSession>;
    /** @deprecated use `CustomerSession$Outbound` instead. */
    type Outbound = CustomerSession$Outbound;
}
export declare function customerSessionToJSON(customerSession: CustomerSession): string;
export declare function customerSessionFromJSON(jsonString: string): SafeParseResult<CustomerSession, SDKValidationError>;
//# sourceMappingURL=customersession.d.ts.map