import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Customer, Customer$Outbound } from "./customer.js";
import { Pagination, Pagination$Outbound } from "./pagination.js";
export type ListResourceCustomer = {
    items: Array<Customer>;
    pagination: Pagination;
};
/** @internal */
export declare const ListResourceCustomer$inboundSchema: z.ZodType<ListResourceCustomer, z.ZodTypeDef, unknown>;
/** @internal */
export type ListResourceCustomer$Outbound = {
    items: Array<Customer$Outbound>;
    pagination: Pagination$Outbound;
};
/** @internal */
export declare const ListResourceCustomer$outboundSchema: z.ZodType<ListResourceCustomer$Outbound, z.ZodTypeDef, ListResourceCustomer>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ListResourceCustomer$ {
    /** @deprecated use `ListResourceCustomer$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ListResourceCustomer, z.ZodTypeDef, unknown>;
    /** @deprecated use `ListResourceCustomer$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ListResourceCustomer$Outbound, z.ZodTypeDef, ListResourceCustomer>;
    /** @deprecated use `ListResourceCustomer$Outbound` instead. */
    type Outbound = ListResourceCustomer$Outbound;
}
export declare function listResourceCustomerToJSON(listResourceCustomer: ListResourceCustomer): string;
export declare function listResourceCustomerFromJSON(jsonString: string): SafeParseResult<ListResourceCustomer, SDKValidationError>;
//# sourceMappingURL=listresourcecustomer.d.ts.map