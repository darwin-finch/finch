import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Address, Address$Outbound } from "./address.js";
import { TaxIDFormat } from "./taxidformat.js";
export type CustomerUpdateMetadata = string | number | boolean;
export type CustomerUpdateTaxId = string | TaxIDFormat;
export type CustomerUpdate = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | null | undefined;
    /**
     * The ID of the customer in your system. This must be unique within the organization. Once set, it can't be updated.
     */
    externalId?: string | null | undefined;
    /**
     * The email address of the customer. This must be unique within the organization.
     */
    email?: string | null | undefined;
    /**
     * The name of the customer.
     */
    name?: string | null | undefined;
    billingAddress?: Address | null | undefined;
    taxId?: Array<string | TaxIDFormat | null> | null | undefined;
};
/** @internal */
export declare const CustomerUpdateMetadata$inboundSchema: z.ZodType<CustomerUpdateMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerUpdateMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const CustomerUpdateMetadata$outboundSchema: z.ZodType<CustomerUpdateMetadata$Outbound, z.ZodTypeDef, CustomerUpdateMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerUpdateMetadata$ {
    /** @deprecated use `CustomerUpdateMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerUpdateMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerUpdateMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerUpdateMetadata$Outbound, z.ZodTypeDef, CustomerUpdateMetadata>;
    /** @deprecated use `CustomerUpdateMetadata$Outbound` instead. */
    type Outbound = CustomerUpdateMetadata$Outbound;
}
export declare function customerUpdateMetadataToJSON(customerUpdateMetadata: CustomerUpdateMetadata): string;
export declare function customerUpdateMetadataFromJSON(jsonString: string): SafeParseResult<CustomerUpdateMetadata, SDKValidationError>;
/** @internal */
export declare const CustomerUpdateTaxId$inboundSchema: z.ZodType<CustomerUpdateTaxId, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerUpdateTaxId$Outbound = string | string;
/** @internal */
export declare const CustomerUpdateTaxId$outboundSchema: z.ZodType<CustomerUpdateTaxId$Outbound, z.ZodTypeDef, CustomerUpdateTaxId>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerUpdateTaxId$ {
    /** @deprecated use `CustomerUpdateTaxId$inboundSchema` instead. */
    const inboundSchema: z.ZodType<string, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerUpdateTaxId$outboundSchema` instead. */
    const outboundSchema: z.ZodType<string, z.ZodTypeDef, string>;
    /** @deprecated use `CustomerUpdateTaxId$Outbound` instead. */
    type Outbound = CustomerUpdateTaxId$Outbound;
}
export declare function customerUpdateTaxIdToJSON(customerUpdateTaxId: CustomerUpdateTaxId): string;
export declare function customerUpdateTaxIdFromJSON(jsonString: string): SafeParseResult<CustomerUpdateTaxId, SDKValidationError>;
/** @internal */
export declare const CustomerUpdate$inboundSchema: z.ZodType<CustomerUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerUpdate$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | null | undefined;
    external_id?: string | null | undefined;
    email?: string | null | undefined;
    name?: string | null | undefined;
    billing_address?: Address$Outbound | null | undefined;
    tax_id?: Array<string | string | null> | null | undefined;
};
/** @internal */
export declare const CustomerUpdate$outboundSchema: z.ZodType<CustomerUpdate$Outbound, z.ZodTypeDef, CustomerUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerUpdate$ {
    /** @deprecated use `CustomerUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerUpdate$Outbound, z.ZodTypeDef, CustomerUpdate>;
    /** @deprecated use `CustomerUpdate$Outbound` instead. */
    type Outbound = CustomerUpdate$Outbound;
}
export declare function customerUpdateToJSON(customerUpdate: CustomerUpdate): string;
export declare function customerUpdateFromJSON(jsonString: string): SafeParseResult<CustomerUpdate, SDKValidationError>;
//# sourceMappingURL=customerupdate.d.ts.map