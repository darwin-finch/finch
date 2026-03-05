import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CustomerProduct, CustomerProduct$Outbound } from "./customerproduct.js";
import { Organization, Organization$Outbound } from "./organization.js";
/**
 * Schema of an organization and related data for customer portal.
 */
export type CustomerOrganization = {
    organization: Organization;
    products: Array<CustomerProduct>;
};
/** @internal */
export declare const CustomerOrganization$inboundSchema: z.ZodType<CustomerOrganization, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerOrganization$Outbound = {
    organization: Organization$Outbound;
    products: Array<CustomerProduct$Outbound>;
};
/** @internal */
export declare const CustomerOrganization$outboundSchema: z.ZodType<CustomerOrganization$Outbound, z.ZodTypeDef, CustomerOrganization>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerOrganization$ {
    /** @deprecated use `CustomerOrganization$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerOrganization, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerOrganization$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerOrganization$Outbound, z.ZodTypeDef, CustomerOrganization>;
    /** @deprecated use `CustomerOrganization$Outbound` instead. */
    type Outbound = CustomerOrganization$Outbound;
}
export declare function customerOrganizationToJSON(customerOrganization: CustomerOrganization): string;
export declare function customerOrganizationFromJSON(jsonString: string): SafeParseResult<CustomerOrganization, SDKValidationError>;
//# sourceMappingURL=customerorganization.d.ts.map