import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Address, Address$Outbound } from "./address.js";
import { CustomerPortalOAuthAccount, CustomerPortalOAuthAccount$Outbound } from "./customerportaloauthaccount.js";
import { TaxIDFormat } from "./taxidformat.js";
export type CustomerPortalCustomerTaxId = string | TaxIDFormat;
export type CustomerPortalCustomer = {
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
    email: string;
    emailVerified: boolean;
    name: string | null;
    billingAddress: Address | null;
    taxId: Array<string | TaxIDFormat | null> | null;
    oauthAccounts: {
        [k: string]: CustomerPortalOAuthAccount;
    };
};
/** @internal */
export declare const CustomerPortalCustomerTaxId$inboundSchema: z.ZodType<CustomerPortalCustomerTaxId, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomerTaxId$Outbound = string | string;
/** @internal */
export declare const CustomerPortalCustomerTaxId$outboundSchema: z.ZodType<CustomerPortalCustomerTaxId$Outbound, z.ZodTypeDef, CustomerPortalCustomerTaxId>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomerTaxId$ {
    /** @deprecated use `CustomerPortalCustomerTaxId$inboundSchema` instead. */
    const inboundSchema: z.ZodType<string, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomerTaxId$outboundSchema` instead. */
    const outboundSchema: z.ZodType<string, z.ZodTypeDef, string>;
    /** @deprecated use `CustomerPortalCustomerTaxId$Outbound` instead. */
    type Outbound = CustomerPortalCustomerTaxId$Outbound;
}
export declare function customerPortalCustomerTaxIdToJSON(customerPortalCustomerTaxId: CustomerPortalCustomerTaxId): string;
export declare function customerPortalCustomerTaxIdFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomerTaxId, SDKValidationError>;
/** @internal */
export declare const CustomerPortalCustomer$inboundSchema: z.ZodType<CustomerPortalCustomer, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomer$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    email: string;
    email_verified: boolean;
    name: string | null;
    billing_address: Address$Outbound | null;
    tax_id: Array<string | string | null> | null;
    oauth_accounts: {
        [k: string]: CustomerPortalOAuthAccount$Outbound;
    };
};
/** @internal */
export declare const CustomerPortalCustomer$outboundSchema: z.ZodType<CustomerPortalCustomer$Outbound, z.ZodTypeDef, CustomerPortalCustomer>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomer$ {
    /** @deprecated use `CustomerPortalCustomer$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomer, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomer$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomer$Outbound, z.ZodTypeDef, CustomerPortalCustomer>;
    /** @deprecated use `CustomerPortalCustomer$Outbound` instead. */
    type Outbound = CustomerPortalCustomer$Outbound;
}
export declare function customerPortalCustomerToJSON(customerPortalCustomer: CustomerPortalCustomer): string;
export declare function customerPortalCustomerFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomer, SDKValidationError>;
//# sourceMappingURL=customerportalcustomer.d.ts.map