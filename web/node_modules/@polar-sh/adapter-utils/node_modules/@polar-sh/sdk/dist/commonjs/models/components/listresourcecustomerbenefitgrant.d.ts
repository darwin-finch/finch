import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CustomerBenefitGrant, CustomerBenefitGrant$Outbound } from "./customerbenefitgrant.js";
import { Pagination, Pagination$Outbound } from "./pagination.js";
export type ListResourceCustomerBenefitGrant = {
    items: Array<CustomerBenefitGrant>;
    pagination: Pagination;
};
/** @internal */
export declare const ListResourceCustomerBenefitGrant$inboundSchema: z.ZodType<ListResourceCustomerBenefitGrant, z.ZodTypeDef, unknown>;
/** @internal */
export type ListResourceCustomerBenefitGrant$Outbound = {
    items: Array<CustomerBenefitGrant$Outbound>;
    pagination: Pagination$Outbound;
};
/** @internal */
export declare const ListResourceCustomerBenefitGrant$outboundSchema: z.ZodType<ListResourceCustomerBenefitGrant$Outbound, z.ZodTypeDef, ListResourceCustomerBenefitGrant>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ListResourceCustomerBenefitGrant$ {
    /** @deprecated use `ListResourceCustomerBenefitGrant$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ListResourceCustomerBenefitGrant, z.ZodTypeDef, unknown>;
    /** @deprecated use `ListResourceCustomerBenefitGrant$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ListResourceCustomerBenefitGrant$Outbound, z.ZodTypeDef, ListResourceCustomerBenefitGrant>;
    /** @deprecated use `ListResourceCustomerBenefitGrant$Outbound` instead. */
    type Outbound = ListResourceCustomerBenefitGrant$Outbound;
}
export declare function listResourceCustomerBenefitGrantToJSON(listResourceCustomerBenefitGrant: ListResourceCustomerBenefitGrant): string;
export declare function listResourceCustomerBenefitGrantFromJSON(jsonString: string): SafeParseResult<ListResourceCustomerBenefitGrant, SDKValidationError>;
//# sourceMappingURL=listresourcecustomerbenefitgrant.d.ts.map