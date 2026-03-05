import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CustomerSubscription, CustomerSubscription$Outbound } from "./customersubscription.js";
import { Pagination, Pagination$Outbound } from "./pagination.js";
export type ListResourceCustomerSubscription = {
    items: Array<CustomerSubscription>;
    pagination: Pagination;
};
/** @internal */
export declare const ListResourceCustomerSubscription$inboundSchema: z.ZodType<ListResourceCustomerSubscription, z.ZodTypeDef, unknown>;
/** @internal */
export type ListResourceCustomerSubscription$Outbound = {
    items: Array<CustomerSubscription$Outbound>;
    pagination: Pagination$Outbound;
};
/** @internal */
export declare const ListResourceCustomerSubscription$outboundSchema: z.ZodType<ListResourceCustomerSubscription$Outbound, z.ZodTypeDef, ListResourceCustomerSubscription>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ListResourceCustomerSubscription$ {
    /** @deprecated use `ListResourceCustomerSubscription$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ListResourceCustomerSubscription, z.ZodTypeDef, unknown>;
    /** @deprecated use `ListResourceCustomerSubscription$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ListResourceCustomerSubscription$Outbound, z.ZodTypeDef, ListResourceCustomerSubscription>;
    /** @deprecated use `ListResourceCustomerSubscription$Outbound` instead. */
    type Outbound = ListResourceCustomerSubscription$Outbound;
}
export declare function listResourceCustomerSubscriptionToJSON(listResourceCustomerSubscription: ListResourceCustomerSubscription): string;
export declare function listResourceCustomerSubscriptionFromJSON(jsonString: string): SafeParseResult<ListResourceCustomerSubscription, SDKValidationError>;
//# sourceMappingURL=listresourcecustomersubscription.d.ts.map