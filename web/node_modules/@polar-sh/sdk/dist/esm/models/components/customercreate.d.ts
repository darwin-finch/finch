import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Address, Address$Outbound } from "./address.js";
import { TaxIDFormat } from "./taxidformat.js";
export type CustomerCreateMetadata = string | number | boolean;
export type CustomerCreateTaxId = string | TaxIDFormat;
export type CustomerCreate = {
    /**
     * Key-value object allowing you to store additional information.
     *
     * @remarks
     *
     * The key must be a string with a maximum length of **40 characters**.
     * The value must be either:
     *
     * * A string with a maximum length of **500 characters**
     * * An integer
     * * A boolean
     *
     * You can store up to **50 key-value pairs**.
     */
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    /**
     * The ID of the customer in your system. This must be unique within the organization. Once set, it can't be updated.
     */
    externalId?: string | null | undefined;
    /**
     * The email address of the customer. This must be unique within the organization.
     */
    email: string;
    /**
     * The name of the customer.
     */
    name?: string | null | undefined;
    billingAddress?: Address | null | undefined;
    taxId?: Array<string | TaxIDFormat | null> | null | undefined;
    /**
     * The ID of the organization owning the customer. **Required unless you use an organization token.**
     */
    organizationId?: string | null | undefined;
};
/** @internal */
export declare const CustomerCreateMetadata$inboundSchema: z.ZodType<CustomerCreateMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerCreateMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CustomerCreateMetadata$outboundSchema: z.ZodType<CustomerCreateMetadata$Outbound, z.ZodTypeDef, CustomerCreateMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerCreateMetadata$ {
    /** @deprecated use `CustomerCreateMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerCreateMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerCreateMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerCreateMetadata$Outbound, z.ZodTypeDef, CustomerCreateMetadata>;
    /** @deprecated use `CustomerCreateMetadata$Outbound` instead. */
    type Outbound = CustomerCreateMetadata$Outbound;
}
export declare function customerCreateMetadataToJSON(customerCreateMetadata: CustomerCreateMetadata): string;
export declare function customerCreateMetadataFromJSON(jsonString: string): SafeParseResult<CustomerCreateMetadata, SDKValidationError>;
/** @internal */
export declare const CustomerCreateTaxId$inboundSchema: z.ZodType<CustomerCreateTaxId, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerCreateTaxId$Outbound = string | string;
/** @internal */
export declare const CustomerCreateTaxId$outboundSchema: z.ZodType<CustomerCreateTaxId$Outbound, z.ZodTypeDef, CustomerCreateTaxId>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerCreateTaxId$ {
    /** @deprecated use `CustomerCreateTaxId$inboundSchema` instead. */
    const inboundSchema: z.ZodType<string, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerCreateTaxId$outboundSchema` instead. */
    const outboundSchema: z.ZodType<string, z.ZodTypeDef, string>;
    /** @deprecated use `CustomerCreateTaxId$Outbound` instead. */
    type Outbound = CustomerCreateTaxId$Outbound;
}
export declare function customerCreateTaxIdToJSON(customerCreateTaxId: CustomerCreateTaxId): string;
export declare function customerCreateTaxIdFromJSON(jsonString: string): SafeParseResult<CustomerCreateTaxId, SDKValidationError>;
/** @internal */
export declare const CustomerCreate$inboundSchema: z.ZodType<CustomerCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerCreate$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    external_id?: string | null | undefined;
    email: string;
    name?: string | null | undefined;
    billing_address?: Address$Outbound | null | undefined;
    tax_id?: Array<string | string | null> | null | undefined;
    organization_id?: string | null | undefined;
};
/** @internal */
export declare const CustomerCreate$outboundSchema: z.ZodType<CustomerCreate$Outbound, z.ZodTypeDef, CustomerCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerCreate$ {
    /** @deprecated use `CustomerCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerCreate$Outbound, z.ZodTypeDef, CustomerCreate>;
    /** @deprecated use `CustomerCreate$Outbound` instead. */
    type Outbound = CustomerCreate$Outbound;
}
export declare function customerCreateToJSON(customerCreate: CustomerCreate): string;
export declare function customerCreateFromJSON(jsonString: string): SafeParseResult<CustomerCreate, SDKValidationError>;
//# sourceMappingURL=customercreate.d.ts.map